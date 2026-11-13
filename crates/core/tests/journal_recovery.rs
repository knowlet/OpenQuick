use openquick_core::journal::{
    JournalError, JournalEvent, JournaledControlPlane, decode_events, encode_events, replay_events,
};
use openquick_core::{
    Approver, ControlPlane, EffectKind, Identity, Outcome, PrincipalId, RequestId, RequestState,
    SessionId, SessionScope, TenantId, ToolRequest,
};

fn scope(tenant: &str, session: &str) -> SessionScope {
    SessionScope::new(
        TenantId::new(tenant).unwrap(),
        SessionId::new(session).unwrap(),
    )
}

fn sample_request(scope: SessionScope, id: &str) -> ToolRequest {
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

#[test]
fn journal_round_trip_encoding() {
    let session = scope("tenant-1", "session-1");
    let request = sample_request(session.clone(), "request-1");
    let events = vec![
        JournalEvent::CreateSession(session.clone()),
        JournalEvent::Submit(request.clone()),
        JournalEvent::Approve {
            request: request.clone(),
            approver: Approver::new(session.clone(), PrincipalId::new("reviewer-1").unwrap()),
        },
        JournalEvent::Begin(request.clone()),
        JournalEvent::Finish {
            request: request.clone(),
            outcome: Outcome::Succeeded,
        },
    ];
    let encoded = encode_events(&events).unwrap();
    assert_eq!(decode_events(&encoded).unwrap(), events);
}

#[test]
fn restart_replays_terminal_lifecycle() {
    let dir = std::env::temp_dir().join(format!("openquick-journal-{}", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let session = scope("tenant-1", "session-1");
    let request = sample_request(session.clone(), "request-1");

    {
        let mut plane = JournaledControlPlane::open(&dir).unwrap();
        plane.create_session(session.clone()).unwrap();
        plane.submit(request.clone()).unwrap();
        plane
            .approve(
                &request,
                Approver::new(session.clone(), PrincipalId::new("reviewer-1").unwrap()),
            )
            .unwrap();
        let permit = plane.begin(&request).unwrap();
        plane.finish(&permit, Outcome::Succeeded).unwrap();
    }

    let (plane, events) = JournaledControlPlane::recover(&dir).unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        plane.state(request.scope(), request.id()).unwrap(),
        RequestState::Succeeded
    );
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn restart_preserves_in_flight_intent() {
    let dir =
        std::env::temp_dir().join(format!("openquick-journal-inflight-{}", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let session = scope("tenant-2", "session-2");
    let request = sample_request(session.clone(), "request-2");

    {
        let mut plane = JournaledControlPlane::open(&dir).unwrap();
        plane.create_session(session.clone()).unwrap();
        plane.submit(request.clone()).unwrap();
        plane
            .approve(
                &request,
                Approver::new(session.clone(), PrincipalId::new("reviewer-1").unwrap()),
            )
            .unwrap();
        let _permit = plane.begin(&request).unwrap();
    }

    let (mut plane, _) = JournaledControlPlane::recover(&dir).unwrap();
    assert_eq!(
        plane.state(request.scope(), request.id()).unwrap(),
        RequestState::Executing
    );
    assert_eq!(plane.outstanding_permits().len(), 1);
    assert!(matches!(
        plane.begin(&request),
        Err(JournalError::Replay(openquick_core::Error::InvalidState(
            RequestState::Executing
        )))
    ));

    let permit = plane
        .outstanding_permits()
        .get(&(request.scope().clone(), request.id().clone()))
        .cloned()
        .expect("in-flight permit after replay");
    plane.finish(&permit, Outcome::Unknown).unwrap();
    assert_eq!(
        plane.state(request.scope(), request.id()).unwrap(),
        RequestState::OutcomeUnknown
    );
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn memory_replay_matches_direct_operations() {
    let session = scope("tenant-3", "session-3");
    let request = sample_request(session.clone(), "request-3");
    let mut direct = ControlPlane::new();
    direct.create_session(session.clone()).unwrap();
    direct.submit(request.clone()).unwrap();
    direct
        .approve(
            &request,
            Approver::new(session.clone(), PrincipalId::new("reviewer-1").unwrap()),
        )
        .unwrap();
    let permit = direct.begin(&request).unwrap();
    direct
        .cancel_request(request.scope(), request.id())
        .unwrap();
    drop(permit);

    let events = vec![
        JournalEvent::CreateSession(session.clone()),
        JournalEvent::Submit(request.clone()),
        JournalEvent::Approve {
            request: request.clone(),
            approver: Approver::new(session.clone(), PrincipalId::new("reviewer-1").unwrap()),
        },
        JournalEvent::Begin(request.clone()),
        JournalEvent::CancelRequest {
            scope: session.clone(),
            id: request.id().clone(),
        },
    ];
    let mut replayed = ControlPlane::new();
    assert!(replay_events(&mut replayed, &events).unwrap().is_empty());
    assert_eq!(
        replayed.state(request.scope(), request.id()).unwrap(),
        direct.state(request.scope(), request.id()).unwrap()
    );
}

#[test]
fn invalid_journal_blob_is_rejected() {
    assert!(matches!(
        decode_events(b"BAD"),
        Err(JournalError::InvalidFormat)
    ));
}

#[test]
fn restart_keeps_identical_request_ids_in_distinct_scopes() {
    for (label, other_scope) in [
        ("session", scope("tenant-1", "session-2")),
        ("tenant", scope("tenant-2", "session-1")),
    ] {
        let path =
            std::env::temp_dir().join(format!("openquick-scoped-{label}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let requests = [
            sample_request(scope("tenant-1", "session-1"), "same-id"),
            sample_request(other_scope, "same-id"),
        ];
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        for request in &requests {
            plane.create_session(request.scope().clone()).unwrap();
            plane.submit(request.clone()).unwrap();
            plane
                .approve(
                    request,
                    Approver::new(
                        request.scope().clone(),
                        PrincipalId::new("reviewer").unwrap(),
                    ),
                )
                .unwrap();
            plane.begin(request).unwrap();
        }
        assert_eq!(plane.outstanding_permits().len(), 2);
        drop(plane);
        let (mut plane, _) = JournaledControlPlane::recover(&path).unwrap();
        assert_eq!(plane.outstanding_permits().len(), 2);
        let permits: Vec<_> = plane.outstanding_permits().values().cloned().collect();
        for permit in permits {
            plane.finish(&permit, Outcome::Succeeded).unwrap();
        }
        drop(plane);
        let (plane, _) = JournaledControlPlane::recover(&path).unwrap();
        for request in &requests {
            assert_eq!(
                plane.state(request.scope(), request.id()).unwrap(),
                RequestState::Succeeded
            );
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn cancellation_only_removes_permits_in_target_scope() {
    for operation in ["session", "request", "revoke"] {
        let path = std::env::temp_dir().join(format!(
            "openquick-cancel-{operation}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let requests = [
            sample_request(scope("tenant-1", "session-1"), "same-id"),
            sample_request(scope("tenant-2", "session-1"), "same-id"),
        ];
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        for request in &requests {
            plane.create_session(request.scope().clone()).unwrap();
            plane.submit(request.clone()).unwrap();
            plane
                .approve(
                    request,
                    Approver::new(
                        request.scope().clone(),
                        PrincipalId::new("reviewer").unwrap(),
                    ),
                )
                .unwrap();
            plane.begin(request).unwrap();
        }
        if operation == "session" {
            plane.cancel_session(requests[0].scope()).unwrap();
        } else if operation == "revoke" {
            plane
                .revoke_approval(requests[0].scope(), requests[0].id())
                .unwrap();
        } else {
            plane
                .cancel_request(requests[0].scope(), requests[0].id())
                .unwrap();
        }
        assert_eq!(plane.outstanding_permits().len(), 1);
        drop(plane);
        let (mut plane, _) = JournaledControlPlane::recover(&path).unwrap();
        assert_eq!(plane.outstanding_permits().len(), 1);
        let permit = plane.outstanding_permits().values().next().unwrap().clone();
        assert_eq!(permit.request(), &requests[1]);
        plane.finish(&permit, Outcome::Succeeded).unwrap();
        drop(plane);
        let (plane, _) = JournaledControlPlane::recover(&path).unwrap();
        assert_eq!(
            plane.state(requests[1].scope(), requests[1].id()).unwrap(),
            RequestState::Succeeded
        );
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn recovery_rejects_partial_record_headers_without_changing_the_file() {
    for valid_prefix in [false, true] {
        for header_bytes in 1..=3 {
            let path = std::env::temp_dir().join(format!(
                "openquick-partial-header-{valid_prefix}-{header_bytes}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            if valid_prefix {
                let mut plane = JournaledControlPlane::open(&path).unwrap();
                plane
                    .create_session(scope("tenant-1", "session-1"))
                    .unwrap();
            }
            let mut bytes = std::fs::read(&path).unwrap_or_default();
            bytes.extend_from_slice(&[0, 0, 0][..header_bytes]);
            std::fs::write(&path, &bytes).unwrap();

            assert!(
                matches!(
                    JournaledControlPlane::recover(&path),
                    Err(JournalError::Io(ref err))
                        if err.kind() == std::io::ErrorKind::UnexpectedEof
                ),
                "accepted a {header_bytes}-byte header (valid prefix: {valid_prefix})"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            std::fs::remove_file(&path).unwrap();
        }
    }
}

#[test]
fn recovery_rejects_truncated_record_payloads_without_changing_the_file() {
    let path =
        std::env::temp_dir().join(format!("openquick-partial-payload-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        plane
            .create_session(scope("tenant-1", "session-1"))
            .unwrap();
    }
    let record = std::fs::read(&path).unwrap();
    for retained_bytes in 4..record.len() {
        let truncated = &record[..retained_bytes];
        std::fs::write(&path, truncated).unwrap();
        assert!(matches!(
            JournaledControlPlane::recover(&path),
            Err(JournalError::Io(ref err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof
        ));
        assert_eq!(std::fs::read(&path).unwrap(), truncated);
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn recovery_accepts_missing_empty_and_complete_journals() {
    let path =
        std::env::temp_dir().join(format!("openquick-clean-boundary-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (plane, events) = JournaledControlPlane::recover(&path).unwrap();
    assert!(events.is_empty());
    drop(plane);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

    let (mut plane, events) = JournaledControlPlane::recover(&path).unwrap();
    assert!(events.is_empty());
    let session = scope("tenant-1", "session-1");
    plane.create_session(session.clone()).unwrap();
    drop(plane);
    let (plane, events) = JournaledControlPlane::recover(&path).unwrap();
    assert_eq!(events, vec![JournalEvent::CreateSession(session.clone())]);
    assert!(!plane.session_cancelled(&session).unwrap());
    drop(plane);
    std::fs::remove_file(&path).unwrap();
}
