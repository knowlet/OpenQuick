//! Append-only journal for control-plane events.
//!
//! A mutation is published only after its record has been appended and synced.
//! The live control plane is left unchanged when that write fails, and the file
//! is truncated back to the previous record boundary. If that boundary cannot be
//! made durable, the in-process journal is poisoned and later mutations fail.
//! Replay re-invokes the public API where possible; in-flight permits are
//! re-issued on reload. This is not a crash-safe or multi-writer store; callers
//! must serialize access. A crash can still leave a torn tail, which recovery
//! rejects instead of repairing.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{
    Approver, ControlPlane, EffectKind, Error, ExecutionPermit, Identity, Outcome, PrincipalId,
    RequestId, RequestState, SessionId, SessionScope, TenantId, ToolRequest,
};

const MAGIC: &[u8; 4] = b"OQJ1";

/// Largest record payload recovery will allocate.
///
/// A valid event is much smaller: argument bytes are capped at 64 KiB and the
/// other request fields are short identifiers or 1 KiB tool and target strings.
/// Length prefixes above this limit are rejected before the payload is allocated.
pub const MAX_RECORD_LEN: usize = 128 * 1024;

type PermitMap = BTreeMap<(SessionScope, RequestId), ExecutionPermit>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalEvent {
    CreateSession(SessionScope),
    Submit(ToolRequest),
    Approve {
        request: ToolRequest,
        approver: Approver,
    },
    Begin(ToolRequest),
    Finish {
        request: ToolRequest,
        outcome: Outcome,
    },
    CancelRequest {
        scope: SessionScope,
        id: RequestId,
    },
    RevokeApproval {
        scope: SessionScope,
        id: RequestId,
    },
    CancelSession(SessionScope),
}

#[derive(Debug)]
pub enum JournalError {
    Io(io::Error),
    InvalidFormat,
    Replay(Error),
    /// `open` was called on a journal that already contains bytes.
    /// Load it with [`JournaledControlPlane::recover`].
    NotEmpty,
    /// An append failed and the previous record boundary could not be made durable.
    /// Further mutations on this instance are rejected.
    Poisoned,
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(formatter, "journal I/O error: {err}"),
            Self::InvalidFormat => formatter.write_str("journal record format is invalid"),
            Self::Replay(err) => write!(formatter, "journal replay failed: {err}"),
            Self::NotEmpty => {
                formatter.write_str("journal is not empty; recover it before appending")
            }
            Self::Poisoned => formatter.write_str(
                "journal is poisoned because its record boundary could not be made durable",
            ),
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Replay(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for JournalError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Trusted in-process control plane that journals each published mutation.
#[derive(Debug)]
pub struct JournaledControlPlane {
    core: ControlPlane,
    journal: JournalWriter,
    outstanding: PermitMap,
    poisoned: bool,
}

impl JournaledControlPlane {
    /// Opens a new empty journal.
    ///
    /// A file that already contains bytes must be loaded with [`Self::recover`].
    /// Opening it here would apply later mutations to a blank in-memory plane
    /// and append them after history that this instance did not replay.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let journal = JournalWriter::open(path)?;
        if journal.len()? != 0 {
            return Err(JournalError::NotEmpty);
        }
        Ok(Self {
            core: ControlPlane::new(),
            journal,
            outstanding: BTreeMap::new(),
            poisoned: false,
        })
    }

    pub fn recover(path: impl AsRef<Path>) -> Result<(Self, Vec<JournalEvent>), JournalError> {
        let events = JournalWriter::read_all(path.as_ref())?;
        let mut core = ControlPlane::new();
        let outstanding = replay_events(&mut core, &events)?;
        let journal = JournalWriter::open(path)?;
        Ok((
            Self {
                core,
                journal,
                outstanding,
                poisoned: false,
            },
            events,
        ))
    }

    /// In-flight permits keyed by full tenant/session scope and request ID.
    pub fn outstanding_permits(&self) -> &BTreeMap<(SessionScope, RequestId), ExecutionPermit> {
        &self.outstanding
    }

    pub fn inner(&self) -> &ControlPlane {
        &self.core
    }

    /// Applies `mutate` to a staged copy and publishes that copy only after
    /// `event` has been appended and synced.
    fn publish<T>(
        &mut self,
        event: &JournalEvent,
        mutate: impl FnOnce(&mut ControlPlane, &mut PermitMap) -> Result<T, Error>,
    ) -> Result<T, JournalError> {
        if self.poisoned {
            return Err(JournalError::Poisoned);
        }
        let mut staged_core = self.core.snapshot();
        let mut staged_permits = self.outstanding.clone();
        let value = mutate(&mut staged_core, &mut staged_permits).map_err(JournalError::Replay)?;
        if let Err(err) = self.journal.append(event) {
            self.poisoned = self.journal.poisoned;
            return Err(err);
        }
        self.core = staged_core;
        self.outstanding = staged_permits;
        Ok(value)
    }

    pub fn create_session(&mut self, scope: SessionScope) -> Result<(), JournalError> {
        self.publish(&JournalEvent::CreateSession(scope.clone()), |core, _| {
            core.create_session(scope)
        })
    }

    pub fn submit(&mut self, request: ToolRequest) -> Result<(), JournalError> {
        self.publish(&JournalEvent::Submit(request.clone()), |core, _| {
            core.submit(request)
        })
    }

    pub fn approve(
        &mut self,
        request: &ToolRequest,
        approver: Approver,
    ) -> Result<(), JournalError> {
        self.publish(
            &JournalEvent::Approve {
                request: request.clone(),
                approver: approver.clone(),
            },
            |core, _| core.approve(request, approver),
        )
    }

    pub fn begin(&mut self, request: &ToolRequest) -> Result<ExecutionPermit, JournalError> {
        self.publish(
            &JournalEvent::Begin(request.clone()),
            |core, outstanding| {
                let permit = core.begin(request)?;
                outstanding.insert(
                    (request.scope().clone(), request.id().clone()),
                    permit.clone(),
                );
                Ok(permit)
            },
        )
    }

    pub fn finish(
        &mut self,
        permit: &ExecutionPermit,
        outcome: Outcome,
    ) -> Result<(), JournalError> {
        let request = permit.request().clone();
        self.publish(
            &JournalEvent::Finish {
                request: request.clone(),
                outcome,
            },
            |core, outstanding| {
                core.finish(permit, outcome)?;
                outstanding.remove(&(request.scope().clone(), request.id().clone()));
                Ok(())
            },
        )
    }

    pub fn cancel_request(
        &mut self,
        scope: &SessionScope,
        id: &RequestId,
    ) -> Result<(), JournalError> {
        self.publish(
            &JournalEvent::CancelRequest {
                scope: scope.clone(),
                id: id.clone(),
            },
            |core, outstanding| {
                core.cancel_request(scope, id)?;
                outstanding.remove(&(scope.clone(), id.clone()));
                Ok(())
            },
        )
    }

    pub fn revoke_approval(
        &mut self,
        scope: &SessionScope,
        id: &RequestId,
    ) -> Result<(), JournalError> {
        self.publish(
            &JournalEvent::RevokeApproval {
                scope: scope.clone(),
                id: id.clone(),
            },
            |core, outstanding| {
                core.revoke_approval(scope, id)?;
                outstanding.remove(&(scope.clone(), id.clone()));
                Ok(())
            },
        )
    }

    pub fn cancel_session(&mut self, scope: &SessionScope) -> Result<(), JournalError> {
        self.publish(
            &JournalEvent::CancelSession(scope.clone()),
            |core, outstanding| {
                core.cancel_session(scope)?;
                outstanding.retain(|(permit_scope, _), _| permit_scope != scope);
                Ok(())
            },
        )
    }

    pub fn state(&self, scope: &SessionScope, id: &RequestId) -> Result<RequestState, Error> {
        self.core.state(scope, id)
    }

    pub fn session_cancelled(&self, scope: &SessionScope) -> Result<bool, Error> {
        self.core.session_cancelled(scope)
    }
}

#[derive(Debug)]
struct JournalWriter {
    file: File,
    poisoned: bool,
    #[cfg(test)]
    faults: AppendFaults,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct AppendFaults {
    /// Write only this many bytes of the next record, then fail with `StorageFull`.
    partial_write: Option<usize>,
    /// Write the full next record, then fail before its `sync_all`.
    fail_sync: bool,
    /// Restoring the pre-append length fails before `set_len`.
    fail_restore_truncate: bool,
    /// `set_len` succeeds and the following boundary sync fails.
    fail_restore_sync: bool,
}

impl JournalWriter {
    fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        if let Some(parent) = path.as_ref().parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true).read(true);
        // Records hold tenant request arguments and principal IDs: create the
        // file owner-only from the start, not at the process umask.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path)?;
        Ok(Self {
            file,
            poisoned: false,
            #[cfg(test)]
            faults: AppendFaults::default(),
        })
    }

    fn len(&self) -> Result<u64, JournalError> {
        Ok(self.file.metadata()?.len())
    }

    fn append(&mut self, event: &JournalEvent) -> Result<(), JournalError> {
        if self.poisoned {
            return Err(JournalError::Poisoned);
        }
        let encoded = encode_event(event)?;
        let len = checked_record_len(encoded.len())?;
        let boundary = self.len()?;
        let mut record = Vec::with_capacity(MAGIC.len() + 4 + encoded.len());
        // The file uses the same framing as `encode_events`: the version
        // header goes in front of the first record, in the same durable write,
        // so an empty journal stays empty and a failed first append restores 0.
        if boundary == 0 {
            record.extend_from_slice(MAGIC);
        }
        record.extend_from_slice(&len.to_be_bytes());
        record.extend_from_slice(&encoded);
        if let Err(err) = self.write_record(&record) {
            return self.fail_append(boundary, err);
        }
        Ok(())
    }

    fn write_record(&mut self, record: &[u8]) -> Result<(), JournalError> {
        #[cfg(test)]
        if let Some(partial) = self.faults.partial_write.take() {
            let written = partial.min(record.len());
            if written > 0 {
                self.file.write_all(&record[..written])?;
            }
            return Err(io::Error::new(io::ErrorKind::StorageFull, "disk full").into());
        }
        self.file.write_all(record)?;
        #[cfg(test)]
        if self.faults.fail_sync {
            self.faults.fail_sync = false;
            return Err(io::Error::new(io::ErrorKind::StorageFull, "sync failed").into());
        }
        self.file.sync_all()?;
        Ok(())
    }

    /// Drops a failed record. Poisons this writer when the previous boundary
    /// cannot be truncated and synced, so later appends cannot extend a torn tail.
    fn fail_append(&mut self, boundary: u64, err: JournalError) -> Result<(), JournalError> {
        match self.restore_boundary(boundary) {
            Ok(()) => Err(err),
            Err(_) => {
                self.poisoned = true;
                Err(JournalError::Poisoned)
            }
        }
    }

    fn restore_boundary(&mut self, boundary: u64) -> Result<(), JournalError> {
        #[cfg(test)]
        if self.faults.fail_restore_truncate {
            self.faults.fail_restore_truncate = false;
            return Err(io::Error::other("journal truncate failed").into());
        }
        self.file.set_len(boundary)?;
        self.file.seek(SeekFrom::Start(boundary))?;
        #[cfg(test)]
        if self.faults.fail_restore_sync {
            self.faults.fail_restore_sync = false;
            return Err(io::Error::new(io::ErrorKind::StorageFull, "truncate sync failed").into());
        }
        self.file.sync_all()?;
        Ok(())
    }

    fn read_all(path: &Path) -> Result<Vec<JournalEvent>, JournalError> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut file = File::open(path)?;
        let mut events = Vec::new();
        if file.metadata()?.len() == 0 {
            return Ok(events);
        }
        // A torn header is a torn first append (UnexpectedEof, like a torn
        // record); any other header is not this format.
        let mut header = [0_u8; 4];
        file.read_exact(&mut header)?;
        if &header != MAGIC {
            return Err(JournalError::InvalidFormat);
        }
        loop {
            let mut len_buf = [0_u8; 4];
            // EOF is valid only before a new record starts. A partial length
            // prefix must not be accepted and later appended to as a clean journal.
            match file.read_exact(&mut len_buf[..1]) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(err.into()),
            }
            file.read_exact(&mut len_buf[1..])?;
            let len = checked_record_len(u32::from_be_bytes(len_buf) as usize)? as usize;
            let mut payload = vec![0_u8; len];
            file.read_exact(&mut payload)?;
            events.push(decode_event(&payload)?);
        }
        Ok(events)
    }
}

fn checked_record_len(len: usize) -> Result<u32, JournalError> {
    let len = u32::try_from(len).map_err(|_| JournalError::InvalidFormat)?;
    if len as usize > MAX_RECORD_LEN {
        return Err(JournalError::InvalidFormat);
    }
    Ok(len)
}

pub fn encode_events(events: &[JournalEvent]) -> Result<Vec<u8>, JournalError> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    for event in events {
        let encoded = encode_event(event)?;
        let len = checked_record_len(encoded.len())?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&encoded);
    }
    Ok(out)
}

pub fn decode_events(bytes: &[u8]) -> Result<Vec<JournalEvent>, JournalError> {
    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Err(JournalError::InvalidFormat);
    }
    let mut cursor = &bytes[MAGIC.len()..];
    let mut events = Vec::new();
    while !cursor.is_empty() {
        if cursor.len() < 4 {
            return Err(JournalError::InvalidFormat);
        }
        let (len_bytes, rest) = cursor.split_at(4);
        let len = checked_record_len(u32::from_be_bytes(len_bytes.try_into().unwrap()) as usize)?
            as usize;
        cursor = rest;
        if cursor.len() < len {
            return Err(JournalError::InvalidFormat);
        }
        let (payload, rest) = cursor.split_at(len);
        events.push(decode_event(payload)?);
        cursor = rest;
    }
    Ok(events)
}

pub fn replay_events(
    core: &mut ControlPlane,
    events: &[JournalEvent],
) -> Result<BTreeMap<(SessionScope, RequestId), ExecutionPermit>, JournalError> {
    let mut permits: BTreeMap<(SessionScope, RequestId), ExecutionPermit> = BTreeMap::new();
    for event in events {
        match event {
            JournalEvent::CreateSession(scope) => {
                core.create_session(scope.clone())
                    .map_err(JournalError::Replay)?;
            }
            JournalEvent::Submit(request) => {
                core.submit(request.clone()).map_err(JournalError::Replay)?;
            }
            JournalEvent::Approve { request, approver } => {
                core.approve(request, approver.clone())
                    .map_err(JournalError::Replay)?;
            }
            JournalEvent::Begin(request) => {
                let permit = core.begin(request).map_err(JournalError::Replay)?;
                permits.insert((request.scope().clone(), request.id().clone()), permit);
            }
            JournalEvent::Finish { request, outcome } => {
                let key = (request.scope().clone(), request.id().clone());
                let permit = permits.get(&key).ok_or(JournalError::InvalidFormat)?;
                if permit.request() != request {
                    return Err(JournalError::InvalidFormat);
                }
                let permit = permits.remove(&key).ok_or(JournalError::InvalidFormat)?;
                core.finish(&permit, *outcome)
                    .map_err(JournalError::Replay)?;
            }
            JournalEvent::CancelRequest { scope, id } => {
                core.cancel_request(scope, id)
                    .map_err(JournalError::Replay)?;
                permits.remove(&(scope.clone(), id.clone()));
            }
            JournalEvent::RevokeApproval { scope, id } => {
                core.revoke_approval(scope, id)
                    .map_err(JournalError::Replay)?;
                permits.remove(&(scope.clone(), id.clone()));
            }
            JournalEvent::CancelSession(scope) => {
                core.cancel_session(scope).map_err(JournalError::Replay)?;
                permits.retain(|(permit_scope, _), _| permit_scope != scope);
            }
        }
    }
    Ok(permits)
}

fn encode_event(event: &JournalEvent) -> Result<Vec<u8>, JournalError> {
    let mut out = Vec::new();
    match event {
        JournalEvent::CreateSession(scope) => {
            out.push(1);
            write_scope(&mut out, scope)?;
        }
        JournalEvent::Submit(request) => {
            out.push(2);
            write_request(&mut out, request)?;
        }
        JournalEvent::Approve { request, approver } => {
            out.push(3);
            write_request(&mut out, request)?;
            write_approver(&mut out, approver)?;
        }
        JournalEvent::Begin(request) => {
            out.push(4);
            write_request(&mut out, request)?;
        }
        JournalEvent::Finish { request, outcome } => {
            out.push(5);
            write_request(&mut out, request)?;
            out.push(match outcome {
                Outcome::Succeeded => 0,
                Outcome::Failed => 1,
                Outcome::Unknown => 2,
            });
        }
        JournalEvent::CancelRequest { scope, id } => {
            out.push(6);
            write_scope(&mut out, scope)?;
            write_identifier(&mut out, id.as_str())?;
        }
        JournalEvent::RevokeApproval { scope, id } => {
            out.push(7);
            write_scope(&mut out, scope)?;
            write_identifier(&mut out, id.as_str())?;
        }
        JournalEvent::CancelSession(scope) => {
            out.push(8);
            write_scope(&mut out, scope)?;
        }
    }
    Ok(out)
}

fn decode_event(bytes: &[u8]) -> Result<JournalEvent, JournalError> {
    let Some((tag, rest)) = bytes.split_first() else {
        return Err(JournalError::InvalidFormat);
    };
    let cursor = rest;
    match *tag {
        1 => {
            let (scope, remaining) = read_scope(cursor)?;
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            Ok(JournalEvent::CreateSession(scope))
        }
        2 => {
            let (request, remaining) = read_request(cursor)?;
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            Ok(JournalEvent::Submit(request))
        }
        3 => {
            let (request, rest) = read_request(cursor)?;
            let (approver, remaining) = read_approver(rest)?;
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            Ok(JournalEvent::Approve { request, approver })
        }
        4 => {
            let (request, remaining) = read_request(cursor)?;
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            Ok(JournalEvent::Begin(request))
        }
        5 => {
            let (request, rest) = read_request(cursor)?;
            let Some((outcome_tag, remaining)) = rest.split_first() else {
                return Err(JournalError::InvalidFormat);
            };
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            let outcome = match *outcome_tag {
                0 => Outcome::Succeeded,
                1 => Outcome::Failed,
                2 => Outcome::Unknown,
                _ => return Err(JournalError::InvalidFormat),
            };
            Ok(JournalEvent::Finish { request, outcome })
        }
        6 => {
            let (scope, rest) = read_scope(cursor)?;
            let (id, remaining) = read_string(rest)?;
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            Ok(JournalEvent::CancelRequest {
                scope,
                id: RequestId::new(id).map_err(|_| JournalError::InvalidFormat)?,
            })
        }
        7 => {
            let (scope, rest) = read_scope(cursor)?;
            let (id, remaining) = read_string(rest)?;
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            Ok(JournalEvent::RevokeApproval {
                scope,
                id: RequestId::new(id).map_err(|_| JournalError::InvalidFormat)?,
            })
        }
        8 => {
            let (scope, remaining) = read_scope(cursor)?;
            if !remaining.is_empty() {
                return Err(JournalError::InvalidFormat);
            }
            Ok(JournalEvent::CancelSession(scope))
        }
        _ => Err(JournalError::InvalidFormat),
    }
}

fn write_scope(out: &mut Vec<u8>, scope: &SessionScope) -> Result<(), JournalError> {
    write_identifier(out, scope.tenant().as_str())?;
    write_identifier(out, scope.session().as_str())?;
    Ok(())
}

fn read_scope(bytes: &[u8]) -> Result<(SessionScope, &[u8]), JournalError> {
    let (tenant, rest) = read_string(bytes)?;
    let (session, remaining) = read_string(rest)?;
    let scope = SessionScope::new(
        TenantId::new(tenant).map_err(|_| JournalError::InvalidFormat)?,
        SessionId::new(session).map_err(|_| JournalError::InvalidFormat)?,
    );
    Ok((scope, remaining))
}

fn write_request(out: &mut Vec<u8>, request: &ToolRequest) -> Result<(), JournalError> {
    write_scope(out, request.scope())?;
    write_identifier(out, request.id().as_str())?;
    write_identity(out, request.identity())?;
    write_identifier(out, request.tool())?;
    write_identifier(out, request.target())?;
    write_bytes(out, request.arguments())?;
    out.push(match request.effect() {
        EffectKind::ReadOnly => 0,
        EffectKind::ExternalWrite => 1,
    });
    Ok(())
}

fn read_request(bytes: &[u8]) -> Result<(ToolRequest, &[u8]), JournalError> {
    let (scope, rest) = read_scope(bytes)?;
    let (id, rest) = read_string(rest)?;
    let (identity, rest) = read_identity(rest)?;
    let (tool, rest) = read_string(rest)?;
    let (target, rest) = read_string(rest)?;
    let (arguments, rest) = read_bytes(rest)?;
    let Some((effect_tag, remaining)) = rest.split_first() else {
        return Err(JournalError::InvalidFormat);
    };
    let effect = match *effect_tag {
        0 => EffectKind::ReadOnly,
        1 => EffectKind::ExternalWrite,
        _ => return Err(JournalError::InvalidFormat),
    };
    let request = ToolRequest::new(
        scope,
        RequestId::new(id).map_err(|_| JournalError::InvalidFormat)?,
        identity,
        tool,
        target,
        arguments,
        effect,
    )
    .map_err(|_| JournalError::InvalidFormat)?;
    Ok((request, remaining))
}

fn write_approver(out: &mut Vec<u8>, approver: &Approver) -> Result<(), JournalError> {
    write_scope(out, approver.scope())?;
    write_identifier(out, approver.principal().as_str())?;
    Ok(())
}

fn read_approver(bytes: &[u8]) -> Result<(Approver, &[u8]), JournalError> {
    let (scope, rest) = read_scope(bytes)?;
    let (principal, remaining) = read_string(rest)?;
    Ok((
        Approver::new(
            scope,
            PrincipalId::new(principal).map_err(|_| JournalError::InvalidFormat)?,
        ),
        remaining,
    ))
}

fn write_identity(out: &mut Vec<u8>, identity: &Identity) -> Result<(), JournalError> {
    match identity {
        Identity::OnBehalfOf { subject } => {
            out.push(0);
            write_identifier(out, subject.as_str())?;
        }
        Identity::Machine { service } => {
            out.push(1);
            write_identifier(out, service.as_str())?;
        }
    }
    Ok(())
}

fn read_identity(bytes: &[u8]) -> Result<(Identity, &[u8]), JournalError> {
    let Some((tag, rest)) = bytes.split_first() else {
        return Err(JournalError::InvalidFormat);
    };
    let (principal, remaining) = read_string(rest)?;
    let principal = PrincipalId::new(principal).map_err(|_| JournalError::InvalidFormat)?;
    let identity = match *tag {
        0 => Identity::OnBehalfOf { subject: principal },
        1 => Identity::Machine { service: principal },
        _ => return Err(JournalError::InvalidFormat),
    };
    Ok((identity, remaining))
}

fn write_identifier(out: &mut Vec<u8>, value: &str) -> Result<(), JournalError> {
    let bytes = value.as_bytes();
    let len = u16::try_from(bytes.len()).map_err(|_| JournalError::InvalidFormat)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), JournalError> {
    let len = u32::try_from(value.len()).map_err(|_| JournalError::InvalidFormat)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn read_string(bytes: &[u8]) -> Result<(String, &[u8]), JournalError> {
    if bytes.len() < 2 {
        return Err(JournalError::InvalidFormat);
    }
    let len = u16::from_be_bytes(bytes[..2].try_into().unwrap()) as usize;
    if len > bytes.len() - 2 {
        return Err(JournalError::InvalidFormat);
    }
    let (value, rest) = bytes[2..].split_at(len);
    let value = std::str::from_utf8(value)
        .map_err(|_| JournalError::InvalidFormat)?
        .to_string();
    Ok((value, rest))
}

fn read_bytes(bytes: &[u8]) -> Result<(Vec<u8>, &[u8]), JournalError> {
    if bytes.len() < 4 {
        return Err(JournalError::InvalidFormat);
    }
    let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    // Compare against what remains rather than `4 + len`, which overflows
    // for a u32::MAX length on 32-bit targets.
    if len > bytes.len() - 4 {
        return Err(JournalError::InvalidFormat);
    }
    let (value, rest) = bytes[4..].split_at(len);
    Ok((value.to_vec(), rest))
}

#[cfg(test)]
mod tests {
    #[test]
    fn inner_lengths_beyond_the_record_are_rejected_without_arithmetic_overflow() {
        use super::{JournalError, read_bytes, read_string};
        let mut record = u32::MAX.to_be_bytes().to_vec();
        record.extend_from_slice(b"short");
        assert!(matches!(
            read_bytes(&record),
            Err(JournalError::InvalidFormat)
        ));
        assert!(matches!(
            read_string(&[0xff, 0xff, b'x']),
            Err(JournalError::InvalidFormat)
        ));
        let (value, rest) = read_bytes(&[0, 0, 0, 2, b'o', b'k', b'!']).unwrap();
        assert_eq!(
            (value.as_slice(), rest),
            (b"ok".as_slice(), b"!".as_slice())
        );
    }
    use super::{JournalError, JournalEvent, JournalWriter, JournaledControlPlane};
    use crate::{
        Approver, EffectKind, Error, Identity, Outcome, PrincipalId, RequestId, RequestState,
        SessionId, SessionScope, TenantId, ToolRequest,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("openquick-journal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn scope(tenant: &str, session: &str) -> SessionScope {
        SessionScope::new(
            TenantId::new(tenant).unwrap(),
            SessionId::new(session).unwrap(),
        )
    }

    fn request(scope: SessionScope, id: &str) -> ToolRequest {
        ToolRequest::new(
            scope,
            RequestId::new(id).unwrap(),
            Identity::OnBehalfOf {
                subject: PrincipalId::new("user-1").unwrap(),
            },
            "send_message",
            "channel-1",
            br#"{"message":"hello"}"#.to_vec(),
            EffectKind::ExternalWrite,
        )
        .unwrap()
    }

    fn storage_full(err: &JournalError) -> bool {
        matches!(err, JournalError::Io(io) if io.kind() == std::io::ErrorKind::StorageFull)
    }

    fn file_len(path: &std::path::Path) -> u64 {
        std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    #[test]
    fn failed_append_keeps_memory_and_journal_on_the_previous_boundary() {
        let path = temp_path("append-rollback");
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        let kept = scope("tenant-1", "session-kept");
        let failed = scope("tenant-1", "session-failed");
        plane.create_session(kept.clone()).unwrap();
        let boundary = file_len(&path);
        assert!(boundary > 0);

        plane.journal.faults.partial_write = Some(1);
        let err = plane.create_session(failed.clone()).unwrap_err();
        assert!(storage_full(&err), "{err}");
        assert!(matches!(
            plane.session_cancelled(&failed),
            Err(Error::UnknownSession)
        ));
        assert!(!plane.session_cancelled(&kept).unwrap());
        assert_eq!(file_len(&path), boundary, "partial append left a torn tail");
        assert!(!plane.poisoned);

        plane.create_session(failed.clone()).unwrap();
        let events = JournalWriter::read_all(&path).unwrap();
        assert_eq!(
            events,
            vec![
                JournalEvent::CreateSession(kept.clone()),
                JournalEvent::CreateSession(failed.clone()),
            ]
        );
        drop(plane);
        let (recovered, events) = JournaledControlPlane::recover(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert!(!recovered.session_cancelled(&kept).unwrap());
        assert!(!recovered.session_cancelled(&failed).unwrap());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn unsynced_append_is_not_published() {
        let path = temp_path("sync-rollback");
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        let kept = scope("tenant-1", "session-kept");
        let failed = scope("tenant-1", "session-unsynced");
        plane.create_session(kept.clone()).unwrap();
        let boundary = file_len(&path);

        plane.journal.faults.fail_sync = true;
        let err = plane.create_session(failed.clone()).unwrap_err();
        assert!(storage_full(&err), "{err}");
        assert!(matches!(
            plane.session_cancelled(&failed),
            Err(Error::UnknownSession)
        ));
        assert_eq!(file_len(&path), boundary);
        assert!(!plane.poisoned);

        plane.create_session(failed.clone()).unwrap();
        assert!(!plane.session_cancelled(&failed).unwrap());
        assert_eq!(JournalWriter::read_all(&path).unwrap().len(), 2);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn unrestorable_boundary_poisons_later_mutations() {
        let path = temp_path("poison-truncate");
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        let kept = scope("tenant-1", "session-kept");
        let failed = scope("tenant-1", "session-poison");
        plane.create_session(kept.clone()).unwrap();
        let boundary = file_len(&path);

        plane.journal.faults.partial_write = Some(1);
        plane.journal.faults.fail_restore_truncate = true;
        assert!(matches!(
            plane.create_session(failed.clone()),
            Err(JournalError::Poisoned)
        ));
        assert!(plane.poisoned);
        assert!(matches!(
            plane.session_cancelled(&failed),
            Err(Error::UnknownSession)
        ));
        assert!(!plane.session_cancelled(&kept).unwrap());
        let torn = file_len(&path);
        assert!(
            torn > boundary,
            "torn tail should remain when truncate fails"
        );

        assert!(matches!(
            plane.create_session(scope("tenant-1", "session-later")),
            Err(JournalError::Poisoned)
        ));
        assert_eq!(file_len(&path), torn, "poisoned journal must not append");
        drop(plane);
        assert!(matches!(
            JournaledControlPlane::recover(&path),
            Err(JournalError::Io(ref err)) if err.kind() == std::io::ErrorKind::UnexpectedEof
        ));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn unsynced_truncate_poisons_even_if_the_length_was_restored() {
        let path = temp_path("poison-sync");
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        let failed = scope("tenant-1", "session-poison-sync");
        plane.journal.faults.fail_sync = true;
        plane.journal.faults.fail_restore_sync = true;
        assert!(matches!(
            plane.create_session(failed.clone()),
            Err(JournalError::Poisoned)
        ));
        assert!(matches!(
            plane.session_cancelled(&failed),
            Err(Error::UnknownSession)
        ));
        assert_eq!(file_len(&path), 0);
        assert!(matches!(
            plane.create_session(failed),
            Err(JournalError::Poisoned)
        ));
        assert_eq!(file_len(&path), 0);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn rejected_mutation_does_not_append() {
        let path = temp_path("reject-no-append");
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        let session = scope("tenant-1", "session-1");
        plane.create_session(session.clone()).unwrap();
        let boundary = file_len(&path);
        assert!(matches!(
            plane.create_session(session),
            Err(JournalError::Replay(Error::SessionExists))
        ));
        assert_eq!(file_len(&path), boundary);
        plane
            .create_session(scope("tenant-1", "session-2"))
            .unwrap();
        assert_eq!(JournalWriter::read_all(&path).unwrap().len(), 2);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn failed_begin_and_finish_do_not_change_permits_or_the_journal() {
        let path = temp_path("permit-rollback");
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        let request = request(scope("tenant-1", "session-1"), "request-1");
        plane.create_session(request.scope().clone()).unwrap();
        plane.submit(request.clone()).unwrap();
        plane
            .approve(
                &request,
                Approver::new(
                    request.scope().clone(),
                    PrincipalId::new("reviewer-1").unwrap(),
                ),
            )
            .unwrap();
        let boundary = file_len(&path);

        plane.journal.faults.partial_write = Some(1);
        assert!(storage_full(&plane.begin(&request).unwrap_err()));
        assert!(plane.outstanding_permits().is_empty());
        assert_eq!(
            plane.state(request.scope(), request.id()).unwrap(),
            RequestState::Approved
        );
        assert_eq!(file_len(&path), boundary);

        let permit = plane.begin(&request).unwrap();
        assert_eq!(plane.outstanding_permits().len(), 1);
        let executing = file_len(&path);
        plane.journal.faults.partial_write = Some(1);
        assert!(storage_full(
            &plane.finish(&permit, Outcome::Succeeded).unwrap_err()
        ));
        assert_eq!(plane.outstanding_permits().len(), 1);
        assert_eq!(
            plane.state(request.scope(), request.id()).unwrap(),
            RequestState::Executing
        );
        assert_eq!(file_len(&path), executing);

        plane.finish(&permit, Outcome::Succeeded).unwrap();
        assert!(plane.outstanding_permits().is_empty());
        drop(plane);
        let (recovered, _) = JournaledControlPlane::recover(&path).unwrap();
        assert_eq!(
            recovered.state(request.scope(), request.id()).unwrap(),
            RequestState::Succeeded
        );
        std::fs::remove_file(&path).unwrap();
    }
}
