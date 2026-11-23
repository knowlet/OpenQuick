use openquick_core::journal::{
    JournalError, JournalEvent, JournaledControlPlane, MAX_RECORD_LEN, decode_events,
    encode_events, replay_events,
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
    // The first four bytes are the OQJ1 header; a header alone is a clean
    // empty journal, so every longer prefix is a torn record.
    assert_eq!(&record[..4], b"OQJ1");
    for retained_bytes in 5..record.len() {
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

#[test]
fn open_rejects_a_non_empty_journal() {
    let path = std::env::temp_dir().join(format!("openquick-open-nonempty-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let session = scope("tenant-1", "session-1");
    {
        let mut plane = JournaledControlPlane::open(&path).unwrap();
        plane.create_session(session.clone()).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.is_empty());
    assert!(matches!(
        JournaledControlPlane::open(&path),
        Err(JournalError::NotEmpty)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    let (mut plane, events) = JournaledControlPlane::recover(&path).unwrap();
    assert_eq!(events, vec![JournalEvent::CreateSession(session.clone())]);
    assert!(matches!(
        plane.create_session(session.clone()),
        Err(JournalError::Replay(openquick_core::Error::SessionExists))
    ));
    let other = scope("tenant-1", "session-2");
    plane.create_session(other.clone()).unwrap();
    drop(plane);
    let (plane, events) = JournaledControlPlane::recover(&path).unwrap();
    assert_eq!(
        events,
        vec![
            JournalEvent::CreateSession(session.clone()),
            JournalEvent::CreateSession(other),
        ]
    );
    assert!(!plane.session_cancelled(&session).unwrap());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn replay_finish_rejects_a_different_recorded_request() {
    let session = scope("tenant-1", "session-1");
    let request = sample_request(session.clone(), "request-1");
    let altered = ToolRequest::new(
        session.clone(),
        request.id().clone(),
        request.identity().clone(),
        request.tool(),
        "channel-2",
        request.arguments().to_vec(),
        request.effect(),
    )
    .unwrap();
    assert_ne!(&altered, &request);
    let prefix = vec![
        JournalEvent::CreateSession(session.clone()),
        JournalEvent::Submit(request.clone()),
        JournalEvent::Approve {
            request: request.clone(),
            approver: Approver::new(session.clone(), PrincipalId::new("reviewer-1").unwrap()),
        },
        JournalEvent::Begin(request.clone()),
    ];
    let mut mismatched = prefix.clone();
    mismatched.push(JournalEvent::Finish {
        request: altered,
        outcome: Outcome::Succeeded,
    });
    let mut core = ControlPlane::new();
    assert!(matches!(
        replay_events(&mut core, &mismatched),
        Err(JournalError::InvalidFormat)
    ));
    assert_eq!(
        core.state(request.scope(), request.id()).unwrap(),
        RequestState::Executing
    );

    let mut matched = prefix;
    matched.push(JournalEvent::Finish {
        request: request.clone(),
        outcome: Outcome::Succeeded,
    });
    let mut core = ControlPlane::new();
    assert!(replay_events(&mut core, &matched).unwrap().is_empty());
    assert_eq!(
        core.state(request.scope(), request.id()).unwrap(),
        RequestState::Succeeded
    );
}

#[test]
fn recovery_rejects_oversized_record_lengths_without_allocating_them() {
    let path =
        std::env::temp_dir().join(format!("openquick-oversize-record-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    for claimed in [u32::MAX, u32::try_from(MAX_RECORD_LEN).unwrap() + 1] {
        let mut bytes = b"OQJ1".to_vec();
        bytes.extend_from_slice(&claimed.to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            matches!(
                JournaledControlPlane::recover(&path),
                Err(JournalError::InvalidFormat)
            ),
            "length {claimed} was not rejected before a short payload read"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(matches!(
            decode_events(&bytes),
            Err(JournalError::InvalidFormat)
        ));
    }

    std::fs::write(&path, [b"OQJ1".as_slice(), &1u32.to_be_bytes()].concat()).unwrap();
    assert!(matches!(
        JournaledControlPlane::recover(&path),
        Err(JournalError::Io(ref err)) if err.kind() == std::io::ErrorKind::UnexpectedEof
    ));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn largest_valid_event_round_trips_under_the_record_cap() {
    let name = "a".repeat(128);
    let field = "b".repeat(1024);
    let session = scope(&name, &name);
    let request = ToolRequest::new(
        session.clone(),
        RequestId::new(&name).unwrap(),
        Identity::OnBehalfOf {
            subject: PrincipalId::new(&name).unwrap(),
        },
        &field,
        &field,
        vec![7_u8; 65_536],
        EffectKind::ExternalWrite,
    )
    .unwrap();
    let event = JournalEvent::Approve {
        request,
        approver: Approver::new(session, PrincipalId::new(&name).unwrap()),
    };
    let encoded = encode_events(std::slice::from_ref(&event)).unwrap();
    let payload_len = encoded.len() - 8;
    assert!(payload_len > 65_536);
    assert!(payload_len <= MAX_RECORD_LEN);
    assert_eq!(decode_events(&encoded).unwrap(), vec![event]);
}

#[test]
fn journaled_control_plane_does_not_expose_an_unjournaled_mutator() {
    let source = include_str!("../src/journal.rs");
    assert!(
        !source.contains("fn inner_mut"),
        "inner_mut would mutate the control plane without appending a journal record"
    );
}

#[test]
fn journal_files_and_the_event_codec_share_one_format() {
    let dir = std::env::temp_dir().join(format!("openquick-interop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let written = dir.join("written.journal");
    let session = scope("tenant-1", "session-1");
    {
        let mut plane = JournaledControlPlane::open(&written).unwrap();
        plane.create_session(session.clone()).unwrap();
        plane.cancel_session(&session).unwrap();
    }
    // A file the writer produced decodes with the public codec ...
    let events = decode_events(&std::fs::read(&written).unwrap()).unwrap();
    assert_eq!(events.len(), 2);
    // ... and codec output recovers as a journal and stays appendable.
    let saved = dir.join("saved.journal");
    std::fs::write(&saved, encode_events(&events).unwrap()).unwrap();
    let (mut plane, recovered) = JournaledControlPlane::recover(&saved).unwrap();
    assert_eq!(recovered, events);
    assert!(plane.session_cancelled(&session).unwrap());
    let other = scope("tenant-1", "session-2");
    plane.create_session(other.clone()).unwrap();
    drop(plane);
    let (_, recovered) = JournaledControlPlane::recover(&saved).unwrap();
    assert_eq!(recovered.last(), Some(&JournalEvent::CreateSession(other)));
    // A journal opened but never written is zero bytes; both paths read it
    // as an empty history.
    let fresh = dir.join("fresh.journal");
    drop(JournaledControlPlane::open(&fresh).unwrap());
    let fresh_bytes = std::fs::read(&fresh).unwrap();
    assert!(fresh_bytes.is_empty());
    assert!(decode_events(&fresh_bytes).unwrap().is_empty());
    assert!(JournaledControlPlane::recover(&fresh).unwrap().1.is_empty());
    // A header with no records is an empty history in both paths.
    assert!(decode_events(b"OQJ1").unwrap().is_empty());
    std::fs::write(&saved, b"OQJ1").unwrap();
    assert!(JournaledControlPlane::recover(&saved).unwrap().1.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn new_journal_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!("openquick-mode-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    // Under a common 022 umask the file would otherwise be 0644.
    let mut plane = JournaledControlPlane::open(&path).unwrap();
    plane
        .create_session(scope("tenant-1", "session-1"))
        .unwrap();
    drop(plane);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "journal mode {mode:o}");
    std::fs::remove_file(&path).unwrap();
}
