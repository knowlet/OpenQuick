//! Append-only journal for control-plane events.
//!
//! Records successful mutations for process restart. Replay re-invokes the public
//! API where possible; in-flight permits are re-issued on reload. This is not a
//! crash-safe or multi-writer store; callers must serialize access.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use crate::{
    Approver, ControlPlane, EffectKind, Error, ExecutionPermit, Identity, Outcome, PrincipalId,
    RequestId, RequestState, SessionId, SessionScope, TenantId, ToolRequest,
};

const MAGIC: &[u8; 4] = b"OQJ1";

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
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(formatter, "journal I/O error: {err}"),
            Self::InvalidFormat => formatter.write_str("journal record format is invalid"),
            Self::Replay(err) => write!(formatter, "journal replay failed: {err}"),
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

use std::fmt;

/// Trusted in-process control plane that appends each successful mutation.
#[derive(Debug)]
pub struct JournaledControlPlane {
    core: ControlPlane,
    journal: JournalWriter,
    outstanding: BTreeMap<(SessionScope, RequestId), ExecutionPermit>,
}

impl JournaledControlPlane {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let journal = JournalWriter::open(path)?;
        Ok(Self {
            core: ControlPlane::new(),
            journal,
            outstanding: BTreeMap::new(),
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

    pub fn inner_mut(&mut self) -> &mut ControlPlane {
        &mut self.core
    }

    fn record(&mut self, event: JournalEvent) -> Result<(), JournalError> {
        self.journal.append(&event)?;
        Ok(())
    }

    pub fn create_session(&mut self, scope: SessionScope) -> Result<(), JournalError> {
        self.core
            .create_session(scope.clone())
            .map_err(JournalError::Replay)?;
        self.record(JournalEvent::CreateSession(scope))?;
        Ok(())
    }

    pub fn submit(&mut self, request: ToolRequest) -> Result<(), JournalError> {
        self.core
            .submit(request.clone())
            .map_err(JournalError::Replay)?;
        self.record(JournalEvent::Submit(request))?;
        Ok(())
    }

    pub fn approve(
        &mut self,
        request: &ToolRequest,
        approver: Approver,
    ) -> Result<(), JournalError> {
        self.core
            .approve(request, approver.clone())
            .map_err(JournalError::Replay)?;
        self.record(JournalEvent::Approve {
            request: request.clone(),
            approver,
        })?;
        Ok(())
    }

    pub fn begin(&mut self, request: &ToolRequest) -> Result<ExecutionPermit, JournalError> {
        let permit = self.core.begin(request).map_err(JournalError::Replay)?;
        self.outstanding.insert(
            (request.scope().clone(), request.id().clone()),
            permit.clone(),
        );
        self.record(JournalEvent::Begin(request.clone()))?;
        Ok(permit)
    }

    pub fn finish(
        &mut self,
        permit: &ExecutionPermit,
        outcome: Outcome,
    ) -> Result<(), JournalError> {
        let request = permit.request().clone();
        self.core
            .finish(permit, outcome)
            .map_err(JournalError::Replay)?;
        self.outstanding
            .remove(&(request.scope().clone(), request.id().clone()));
        self.record(JournalEvent::Finish { request, outcome })?;
        Ok(())
    }

    pub fn cancel_request(
        &mut self,
        scope: &SessionScope,
        id: &RequestId,
    ) -> Result<(), JournalError> {
        self.core
            .cancel_request(scope, id)
            .map_err(JournalError::Replay)?;
        self.outstanding.remove(&(scope.clone(), id.clone()));
        self.record(JournalEvent::CancelRequest {
            scope: scope.clone(),
            id: id.clone(),
        })?;
        Ok(())
    }

    pub fn revoke_approval(
        &mut self,
        scope: &SessionScope,
        id: &RequestId,
    ) -> Result<(), JournalError> {
        self.core
            .revoke_approval(scope, id)
            .map_err(JournalError::Replay)?;
        self.outstanding.remove(&(scope.clone(), id.clone()));
        self.record(JournalEvent::RevokeApproval {
            scope: scope.clone(),
            id: id.clone(),
        })?;
        Ok(())
    }

    pub fn cancel_session(&mut self, scope: &SessionScope) -> Result<(), JournalError> {
        self.core
            .cancel_session(scope)
            .map_err(JournalError::Replay)?;
        self.outstanding
            .retain(|(permit_scope, _), _| permit_scope != scope);
        self.record(JournalEvent::CancelSession(scope.clone()))?;
        Ok(())
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
}

impl JournalWriter {
    fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        if let Some(parent) = path.as_ref().parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        Ok(Self { file })
    }

    fn append(&mut self, event: &JournalEvent) -> Result<(), JournalError> {
        let encoded = encode_event(event)?;
        let len = u32::try_from(encoded.len()).map_err(|_| JournalError::InvalidFormat)?;
        self.file.write_all(&len.to_be_bytes())?;
        self.file.write_all(&encoded)?;
        self.file.sync_all()?;
        Ok(())
    }

    fn read_all(path: &Path) -> Result<Vec<JournalEvent>, JournalError> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut file = File::open(path)?;
        let mut events = Vec::new();
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
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0_u8; len];
            file.read_exact(&mut payload)?;
            events.push(decode_event(&payload)?);
        }
        Ok(events)
    }
}

pub fn encode_events(events: &[JournalEvent]) -> Result<Vec<u8>, JournalError> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    for event in events {
        let encoded = encode_event(event)?;
        let len = u32::try_from(encoded.len()).map_err(|_| JournalError::InvalidFormat)?;
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
        let len = u32::from_be_bytes(len_bytes.try_into().unwrap()) as usize;
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
                let permit = permits
                    .remove(&(request.scope().clone(), request.id().clone()))
                    .ok_or(JournalError::InvalidFormat)?;
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
    if bytes.len() < 2 + len {
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
    if bytes.len() < 4 + len {
        return Err(JournalError::InvalidFormat);
    }
    let (value, rest) = bytes[4..].split_at(len);
    Ok((value.to_vec(), rest))
}
