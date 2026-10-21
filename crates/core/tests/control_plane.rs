use openquick_core::{
    Approver, ControlPlane, EffectKind, Error, Identity, Outcome, PrincipalId, RequestId,
    RequestState, SessionId, SessionScope, TenantId, ToolRequest,
};

fn scope(tenant: &str, session: &str) -> SessionScope {
    SessionScope::new(
        TenantId::new(tenant).unwrap(),
        SessionId::new(session).unwrap(),
    )
}

fn request(scope: SessionScope) -> ToolRequest {
    ToolRequest::new(
        scope,
        RequestId::new("request-1").unwrap(),
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

fn approver(scope: SessionScope) -> Approver {
    Approver::new(scope, PrincipalId::new("reviewer-1").unwrap())
}

fn pending() -> (ControlPlane, ToolRequest) {
    let mut core = ControlPlane::new();
    let request = request(scope("tenant-1", "session-1"));
    core.create_session(request.scope().clone()).unwrap();
    core.submit(request.clone()).unwrap();
    (core, request)
}

fn approved() -> (ControlPlane, ToolRequest) {
    let (mut core, request) = pending();
    core.approve(&request, approver(request.scope().clone()))
        .unwrap();
    (core, request)
}

#[test]
fn only_explicit_approval_allows_one_execution() {
    let (mut core, request) = pending();
    assert_eq!(core.begin(&request).unwrap_err(), Error::ApprovalRequired);
    let author = approver(request.scope().clone());
    core.approve(&request, author.clone()).unwrap();
    let recorded = core
        .approval(request.scope(), request.id())
        .unwrap()
        .unwrap();
    assert_eq!(recorded.request(), &request);
    assert_eq!(recorded.approver(), &author);
    let permit = core.begin(&request).unwrap();
    assert_eq!(permit.request(), &request);
    assert_eq!(
        core.begin(&request).unwrap_err(),
        Error::InvalidState(RequestState::Executing)
    );
    core.finish(&permit, Outcome::Succeeded).unwrap();
    assert_eq!(
        core.state(request.scope(), request.id()).unwrap(),
        RequestState::Succeeded
    );
    assert_eq!(
        core.finish(&permit, Outcome::Succeeded),
        Err(Error::InvalidState(RequestState::Succeeded))
    );
}

#[test]
fn approval_and_begin_reject_every_request_substitution() {
    let (mut core, request) = pending();
    let alternate_identity = Identity::Machine {
        service: PrincipalId::new("user-1").unwrap(),
    };
    let substitutions = [
        ToolRequest::new(
            request.scope().clone(),
            request.id().clone(),
            request.identity().clone(),
            "delete_message",
            request.target(),
            request.arguments(),
            request.effect(),
        )
        .unwrap(),
        ToolRequest::new(
            request.scope().clone(),
            request.id().clone(),
            request.identity().clone(),
            request.tool(),
            "channel-2",
            request.arguments(),
            request.effect(),
        )
        .unwrap(),
        ToolRequest::new(
            request.scope().clone(),
            request.id().clone(),
            request.identity().clone(),
            request.tool(),
            request.target(),
            b"different arguments".to_vec(),
            request.effect(),
        )
        .unwrap(),
        ToolRequest::new(
            request.scope().clone(),
            request.id().clone(),
            Identity::OnBehalfOf {
                subject: PrincipalId::new("another-user").unwrap(),
            },
            request.tool(),
            request.target(),
            request.arguments(),
            request.effect(),
        )
        .unwrap(),
        ToolRequest::new(
            request.scope().clone(),
            request.id().clone(),
            alternate_identity,
            request.tool(),
            request.target(),
            request.arguments(),
            request.effect(),
        )
        .unwrap(),
        ToolRequest::new(
            request.scope().clone(),
            request.id().clone(),
            request.identity().clone(),
            request.tool(),
            request.target(),
            request.arguments(),
            EffectKind::ReadOnly,
        )
        .unwrap(),
    ];
    for changed in &substitutions {
        assert_eq!(
            core.approve(changed, approver(request.scope().clone())),
            Err(Error::RequestMismatch)
        );
    }
    core.approve(&request, approver(request.scope().clone()))
        .unwrap();
    for changed in &substitutions {
        assert_eq!(core.begin(changed).unwrap_err(), Error::RequestMismatch);
    }
    assert!(core.begin(&request).is_ok());
}

#[test]
fn machine_identity_has_its_own_explicit_approval_path() {
    let mut core = ControlPlane::new();
    let scope = scope("tenant-1", "machine-session");
    let request = ToolRequest::new(
        scope.clone(),
        RequestId::new("request-1").unwrap(),
        Identity::Machine {
            service: PrincipalId::new("service-1").unwrap(),
        },
        "read_report",
        "report-1",
        b"{}".to_vec(),
        EffectKind::ReadOnly,
    )
    .unwrap();
    core.create_session(scope.clone()).unwrap();
    core.submit(request.clone()).unwrap();
    assert_eq!(core.begin(&request).unwrap_err(), Error::ApprovalRequired);
    core.approve(&request, approver(scope)).unwrap();
    let permit = core.begin(&request).unwrap();
    core.finish(&permit, Outcome::Succeeded).unwrap();
    assert_eq!(
        core.state(request.scope(), request.id()).unwrap(),
        RequestState::Succeeded
    );
}

#[test]
fn read_only_uncertainty_is_also_terminal() {
    let mut core = ControlPlane::new();
    let scope = scope("tenant-1", "read-session");
    let request = ToolRequest::new(
        scope.clone(),
        RequestId::new("request-1").unwrap(),
        Identity::OnBehalfOf {
            subject: PrincipalId::new("user-1").unwrap(),
        },
        "read_report",
        "report-1",
        b"{}".to_vec(),
        EffectKind::ReadOnly,
    )
    .unwrap();
    core.create_session(scope.clone()).unwrap();
    core.submit(request.clone()).unwrap();
    core.approve(&request, approver(scope)).unwrap();
    let permit = core.begin(&request).unwrap();
    core.finish(&permit, Outcome::Unknown).unwrap();
    assert_eq!(
        core.begin(&request).unwrap_err(),
        Error::InvalidState(RequestState::OutcomeUnknown)
    );
    assert_eq!(core.submit(request.clone()), Err(Error::DuplicateRequest));
    assert_eq!(
        core.approve(&request, approver(request.scope().clone())),
        Err(Error::InvalidState(RequestState::OutcomeUnknown))
    );
}

#[test]
fn tenant_and_session_scopes_do_not_share_approvals() {
    let (mut core, original) = approved();
    for other_scope in [
        scope("tenant-2", "session-1"),
        scope("tenant-1", "session-2"),
    ] {
        let other = request(other_scope.clone());
        core.create_session(other_scope).unwrap();
        core.submit(other.clone()).unwrap();
        assert_eq!(
            core.approve(&other, approver(original.scope().clone())),
            Err(Error::ScopeMismatch)
        );
        assert_eq!(core.begin(&other).unwrap_err(), Error::ApprovalRequired);
    }
    assert!(core.begin(&original).is_ok());
}

#[test]
fn cancelling_or_revoking_queued_work_blocks_approval_and_execution() {
    for cancel in [true, false] {
        for already_approved in [true, false] {
            let (mut core, request) = if already_approved {
                approved()
            } else {
                pending()
            };
            let expected = if cancel {
                core.cancel_request(request.scope(), request.id()).unwrap();
                RequestState::Cancelled
            } else {
                core.revoke_approval(request.scope(), request.id()).unwrap();
                RequestState::Revoked
            };
            assert_eq!(
                core.begin(&request).unwrap_err(),
                Error::InvalidState(expected)
            );
            assert_eq!(
                core.approve(&request, approver(request.scope().clone())),
                Err(Error::InvalidState(expected))
            );
            assert_eq!(core.submit(request.clone()), Err(Error::DuplicateRequest));
        }
    }
}

#[test]
fn cancelling_session_blocks_existing_and_future_work() {
    let (mut core, request) = approved();
    core.cancel_session(request.scope()).unwrap();
    assert!(core.session_cancelled(request.scope()).unwrap());
    assert_eq!(core.begin(&request).unwrap_err(), Error::SessionCancelled);
    assert_eq!(core.submit(request.clone()), Err(Error::SessionCancelled));
    assert_eq!(
        core.approve(&request, approver(request.scope().clone())),
        Err(Error::SessionCancelled)
    );
    assert_eq!(
        core.create_session(request.scope().clone()),
        Err(Error::SessionExists)
    );
}

#[test]
fn unknown_external_write_never_retries_or_reapproves() {
    let (mut core, request) = approved();
    let permit = core.begin(&request).unwrap();
    core.finish(&permit, Outcome::Unknown).unwrap();
    assert_eq!(
        core.state(request.scope(), request.id()).unwrap(),
        RequestState::OutcomeUnknown
    );
    assert_eq!(
        core.begin(&request).unwrap_err(),
        Error::InvalidState(RequestState::OutcomeUnknown)
    );
    assert_eq!(
        core.approve(&request, approver(request.scope().clone())),
        Err(Error::InvalidState(RequestState::OutcomeUnknown))
    );
    assert_eq!(core.submit(request.clone()), Err(Error::DuplicateRequest));
    assert_eq!(
        core.finish(&permit, Outcome::Succeeded),
        Err(Error::InvalidState(RequestState::OutcomeUnknown))
    );
}

#[test]
fn in_flight_cancellation_or_revocation_preserves_uncertainty() {
    for cancellation in [0, 1, 2] {
        let (mut core, request) = approved();
        let permit = core.begin(&request).unwrap();
        match cancellation {
            0 => core.cancel_request(request.scope(), request.id()).unwrap(),
            1 => core.revoke_approval(request.scope(), request.id()).unwrap(),
            _ => core.cancel_session(request.scope()).unwrap(),
        }
        assert_eq!(
            core.state(request.scope(), request.id()).unwrap(),
            RequestState::OutcomeUnknown
        );
        assert!(core.begin(&request).is_err());
        assert!(core.finish(&permit, Outcome::Succeeded).is_err());
    }
}

#[test]
fn permits_cannot_cross_control_plane_instances() {
    let (mut first, request) = approved();
    let (mut second, _) = approved();
    let first_permit = first.begin(&request).unwrap();
    let second_permit = second.begin(&request).unwrap();
    assert_eq!(
        second.finish(&first_permit, Outcome::Succeeded),
        Err(Error::InvalidPermit)
    );
    assert_eq!(
        first.finish(&second_permit, Outcome::Succeeded),
        Err(Error::InvalidPermit)
    );
    first.finish(&first_permit, Outcome::Succeeded).unwrap();
    second.finish(&second_permit, Outcome::Failed).unwrap();
}

#[test]
fn confirmed_failure_is_terminal_too() {
    let (mut core, request) = approved();
    let permit = core.begin(&request).unwrap();
    core.finish(&permit, Outcome::Failed).unwrap();
    assert_eq!(
        core.begin(&request).unwrap_err(),
        Error::InvalidState(RequestState::Failed)
    );
    assert_eq!(core.submit(request.clone()), Err(Error::DuplicateRequest));
}

#[test]
fn invalid_identifiers_and_uncreated_sessions_fail_closed() {
    for invalid in ["", "has space", "line\nbreak", "../session", "日本語"] {
        assert!(SessionId::new(invalid).is_err());
    }
    assert!(TenantId::new("a".repeat(129)).is_err());
    let mut core = ControlPlane::new();
    assert_eq!(
        core.submit(request(scope("tenant", "missing"))),
        Err(Error::UnknownSession)
    );
}
