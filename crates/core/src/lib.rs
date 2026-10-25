//! An in-memory control plane for trusted callers.
//!
//! This crate does not authenticate approvers, contact tools, persist state, or
//! isolate workers. The caller must establish identity and authorization before
//! recording an approval. Credentials must never be included in a request.
//! Approval compares the complete request bytes; no normalization is performed.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, Error> {
                let value = value.into();
                if value.is_empty()
                    || value.len() > 128
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:@".contains(&byte))
                {
                    return Err(Error::InvalidIdentifier(stringify!($name)));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(TenantId);
identifier!(SessionId);
identifier!(RequestId);
identifier!(PrincipalId);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionScope {
    tenant: TenantId,
    session: SessionId,
}

impl SessionScope {
    pub fn new(tenant: TenantId, session: SessionId) -> Self {
        Self { tenant, session }
    }

    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }
}

/// Identity claims supplied by a trusted caller, never tokens or credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    OnBehalfOf { subject: PrincipalId },
    Machine { service: PrincipalId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectKind {
    ReadOnly,
    ExternalWrite,
}

/// A complete immutable approval subject. Argument bytes must be credential-free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRequest {
    scope: SessionScope,
    id: RequestId,
    identity: Identity,
    tool: String,
    target: String,
    arguments: Vec<u8>,
    effect: EffectKind,
}

impl ToolRequest {
    pub fn new(
        scope: SessionScope,
        id: RequestId,
        identity: Identity,
        tool: impl Into<String>,
        target: impl Into<String>,
        arguments: impl Into<Vec<u8>>,
        effect: EffectKind,
    ) -> Result<Self, Error> {
        let tool = tool.into();
        let target = target.into();
        let arguments = arguments.into();
        for (field, value) in [("tool", &tool), ("target", &target)] {
            if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
                return Err(Error::InvalidRequestField(field));
            }
        }
        if arguments.len() > 65_536 {
            return Err(Error::InvalidRequestField("arguments"));
        }
        Ok(Self {
            scope,
            id,
            identity,
            tool,
            target,
            arguments,
            effect,
        })
    }

    pub fn scope(&self) -> &SessionScope {
        &self.scope
    }

    pub fn id(&self) -> &RequestId {
        &self.id
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn tool(&self) -> &str {
        &self.tool
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn arguments(&self) -> &[u8] {
        &self.arguments
    }

    pub fn effect(&self) -> EffectKind {
        self.effect
    }
}

/// Audit attribution only. Construction does not authenticate this principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approver {
    scope: SessionScope,
    principal: PrincipalId,
}

impl Approver {
    pub fn new(scope: SessionScope, principal: PrincipalId) -> Self {
        Self { scope, principal }
    }

    pub fn scope(&self) -> &SessionScope {
        &self.scope
    }

    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    request: ToolRequest,
    approver: Approver,
}

impl Approval {
    pub fn request(&self) -> &ToolRequest {
        &self.request
    }

    pub fn approver(&self) -> &Approver {
        &self.approver
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestState {
    PendingApproval,
    Approved,
    Executing,
    Succeeded,
    Failed,
    OutcomeUnknown,
    Cancelled,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Unknown,
}

/// Issued once by `begin`; valid only for its originating control-plane instance.
///
/// A permit is an in-process lifecycle handle, not a serialized authorization
/// capability. It does not by itself authorize a future gateway to dispatch.
#[derive(Debug)]
pub struct ExecutionPermit {
    issuer: Arc<()>,
    request: ToolRequest,
}

impl ExecutionPermit {
    pub fn request(&self) -> &ToolRequest {
        &self.request
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidIdentifier(&'static str),
    InvalidRequestField(&'static str),
    SessionExists,
    UnknownSession,
    SessionCancelled,
    DuplicateRequest,
    UnknownRequest,
    ScopeMismatch,
    RequestMismatch,
    ApprovalRequired,
    InvalidState(RequestState),
    InvalidPermit,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(kind) => write!(formatter, "invalid {kind}"),
            Self::InvalidRequestField(field) => write!(formatter, "invalid request {field}"),
            Self::SessionExists => formatter.write_str("session already exists"),
            Self::UnknownSession => formatter.write_str("unknown session"),
            Self::SessionCancelled => formatter.write_str("session is cancelled"),
            Self::DuplicateRequest => formatter.write_str("request ID was already used"),
            Self::UnknownRequest => formatter.write_str("unknown request"),
            Self::ScopeMismatch => formatter.write_str("approval scope does not match request"),
            Self::RequestMismatch => formatter.write_str("request differs from recorded request"),
            Self::ApprovalRequired => formatter.write_str("explicit approval is required"),
            Self::InvalidState(state) => write!(formatter, "operation denied in state {state:?}"),
            Self::InvalidPermit => {
                formatter.write_str("execution permit belongs to another instance")
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
struct Entry {
    request: ToolRequest,
    approval: Option<Approval>,
    state: RequestState,
}

#[derive(Debug, Default)]
struct Session {
    cancelled: bool,
    requests: BTreeMap<RequestId, Entry>,
}

/// Single-owner in-memory state; no durability or distributed exclusion claim.
#[derive(Debug, Default)]
pub struct ControlPlane {
    issuer: Arc<()>,
    sessions: BTreeMap<SessionScope, Session>,
}

impl ControlPlane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_session(&mut self, scope: SessionScope) -> Result<(), Error> {
        if self.sessions.contains_key(&scope) {
            return Err(Error::SessionExists);
        }
        self.sessions.insert(scope, Session::default());
        Ok(())
    }

    pub fn submit(&mut self, request: ToolRequest) -> Result<(), Error> {
        let session = self.active_session_mut(&request.scope)?;
        if session.requests.contains_key(&request.id) {
            return Err(Error::DuplicateRequest);
        }
        session.requests.insert(
            request.id.clone(),
            Entry {
                request,
                approval: None,
                state: RequestState::PendingApproval,
            },
        );
        Ok(())
    }

    /// Records approval supplied by a separately authenticated trusted caller.
    pub fn approve(&mut self, request: &ToolRequest, approver: Approver) -> Result<(), Error> {
        if approver.scope != request.scope {
            return Err(Error::ScopeMismatch);
        }
        let entry = self.expected_entry_mut(request)?;
        if entry.state != RequestState::PendingApproval {
            return Err(Error::InvalidState(entry.state));
        }
        entry.approval = Some(Approval {
            request: request.clone(),
            approver,
        });
        entry.state = RequestState::Approved;
        Ok(())
    }

    /// Consumes the recorded approval once. This method performs no tool I/O.
    pub fn begin(&mut self, request: &ToolRequest) -> Result<ExecutionPermit, Error> {
        let entry = self.expected_entry_mut(request)?;
        if entry.state == RequestState::PendingApproval {
            return Err(Error::ApprovalRequired);
        }
        if entry.state != RequestState::Approved {
            return Err(Error::InvalidState(entry.state));
        }
        if !entry
            .approval
            .as_ref()
            .is_some_and(|approval| approval.request == *request)
        {
            return Err(Error::ApprovalRequired);
        }
        entry.state = RequestState::Executing;
        Ok(ExecutionPermit {
            issuer: Arc::clone(&self.issuer),
            request: request.clone(),
        })
    }

    /// Records a known result or uncertainty; all three outcomes are terminal.
    ///
    /// `Failed` means a confirmed failure. Transport loss, timeout, or a lost
    /// response to a possible external write must use `Unknown` instead.
    pub fn finish(&mut self, permit: &ExecutionPermit, outcome: Outcome) -> Result<(), Error> {
        if !Arc::ptr_eq(&self.issuer, &permit.issuer) {
            return Err(Error::InvalidPermit);
        }
        let entry = self.expected_entry_mut(&permit.request)?;
        if entry.state != RequestState::Executing {
            return Err(Error::InvalidState(entry.state));
        }
        entry.state = match outcome {
            Outcome::Succeeded => RequestState::Succeeded,
            Outcome::Failed => RequestState::Failed,
            Outcome::Unknown => RequestState::OutcomeUnknown,
        };
        Ok(())
    }

    pub fn cancel_request(&mut self, scope: &SessionScope, id: &RequestId) -> Result<(), Error> {
        let entry = self.entry_mut(scope, id)?;
        entry.state = match entry.state {
            RequestState::PendingApproval | RequestState::Approved => RequestState::Cancelled,
            RequestState::Executing => RequestState::OutcomeUnknown,
            state => return Err(Error::InvalidState(state)),
        };
        Ok(())
    }

    pub fn revoke_approval(&mut self, scope: &SessionScope, id: &RequestId) -> Result<(), Error> {
        let entry = self.entry_mut(scope, id)?;
        entry.state = match entry.state {
            RequestState::PendingApproval | RequestState::Approved => RequestState::Revoked,
            RequestState::Executing => RequestState::OutcomeUnknown,
            state => return Err(Error::InvalidState(state)),
        };
        Ok(())
    }

    /// Cancels queued work; in-flight work remains uncertain, never retryable.
    /// Cancellation cannot undo an external side effect already in progress.
    pub fn cancel_session(&mut self, scope: &SessionScope) -> Result<(), Error> {
        let session = self.sessions.get_mut(scope).ok_or(Error::UnknownSession)?;
        session.cancelled = true;
        for entry in session.requests.values_mut() {
            entry.state = match entry.state {
                RequestState::PendingApproval | RequestState::Approved => RequestState::Cancelled,
                RequestState::Executing => RequestState::OutcomeUnknown,
                state => state,
            };
        }
        Ok(())
    }

    pub fn state(&self, scope: &SessionScope, id: &RequestId) -> Result<RequestState, Error> {
        Ok(self.entry(scope, id)?.state)
    }

    pub fn approval(
        &self,
        scope: &SessionScope,
        id: &RequestId,
    ) -> Result<Option<&Approval>, Error> {
        Ok(self.entry(scope, id)?.approval.as_ref())
    }

    pub fn session_cancelled(&self, scope: &SessionScope) -> Result<bool, Error> {
        Ok(self
            .sessions
            .get(scope)
            .ok_or(Error::UnknownSession)?
            .cancelled)
    }

    fn active_session_mut(&mut self, scope: &SessionScope) -> Result<&mut Session, Error> {
        let session = self.sessions.get_mut(scope).ok_or(Error::UnknownSession)?;
        if session.cancelled {
            return Err(Error::SessionCancelled);
        }
        Ok(session)
    }

    fn entry_mut(&mut self, scope: &SessionScope, id: &RequestId) -> Result<&mut Entry, Error> {
        self.active_session_mut(scope)?
            .requests
            .get_mut(id)
            .ok_or(Error::UnknownRequest)
    }

    fn expected_entry_mut(&mut self, request: &ToolRequest) -> Result<&mut Entry, Error> {
        let entry = self.entry_mut(&request.scope, &request.id)?;
        if entry.request != *request {
            return Err(Error::RequestMismatch);
        }
        Ok(entry)
    }

    fn entry(&self, scope: &SessionScope, id: &RequestId) -> Result<&Entry, Error> {
        self.sessions
            .get(scope)
            .ok_or(Error::UnknownSession)?
            .requests
            .get(id)
            .ok_or(Error::UnknownRequest)
    }
}
