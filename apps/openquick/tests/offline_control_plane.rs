//! Offline composition tests of the public core API and the actual closed gateway.
//!
//! The mutex belongs to this trusted caller: the core itself is single-owner.
//! These tests do not establish remote dispatch or distributed exclusion.

use openquick_core::{
    Approver, ControlPlane, EffectKind, Error, ExecutionPermit, Identity, Outcome, PrincipalId,
    RequestId, RequestState, SessionId, SessionScope, TenantId, ToolRequest,
};
use openquick_gateway::{Gateway, IntegrationUnavailable};
use std::sync::{Barrier, Mutex, mpsc};
use std::thread;

const CONTENDERS: usize = 16;
const RACE_ITERATIONS: usize = 32;

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
        "example.write",
        "offline-target",
        br#"{"value":1}"#.to_vec(),
        EffectKind::ExternalWrite,
    )
    .unwrap()
}

fn approver(request: &ToolRequest) -> Approver {
    Approver::new(
        request.scope().clone(),
        PrincipalId::new("reviewer-1").unwrap(),
    )
}

fn approved(request: &ToolRequest) -> ControlPlane {
    let mut core = ControlPlane::new();
    core.create_session(request.scope().clone()).unwrap();
    core.submit(request.clone()).unwrap();
    core.approve(request, approver(request)).unwrap();
    core
}

fn dispatch(permit: &ExecutionPermit) {
    assert_eq!(Gateway.dispatch(permit), Err(IntegrationUnavailable));
}

fn assert_terminal(
    core: &mut ControlPlane,
    request: &ToolRequest,
    permit: Option<&ExecutionPermit>,
    state: RequestState,
) {
    let cancelled_session = core.session_cancelled(request.scope()).unwrap();
    let denied = if cancelled_session {
        Error::SessionCancelled
    } else {
        Error::InvalidState(state)
    };
    assert_eq!(core.begin(request).unwrap_err(), denied);
    assert_eq!(
        core.approve(request, approver(request)),
        Err(denied.clone())
    );
    assert_eq!(
        core.cancel_request(request.scope(), request.id()),
        Err(denied.clone())
    );
    assert_eq!(
        core.revoke_approval(request.scope(), request.id()),
        Err(denied.clone())
    );
    if let Some(permit) = permit {
        for outcome in [Outcome::Succeeded, Outcome::Failed, Outcome::Unknown] {
            assert_eq!(core.finish(permit, outcome), Err(denied.clone()));
        }
        // Even a retained permit never opens the unavailable integration.
        dispatch(permit);
    }
    assert_eq!(
        core.submit(request.clone()),
        Err(if cancelled_session {
            Error::SessionCancelled
        } else {
            Error::DuplicateRequest
        })
    );
    assert_eq!(core.state(request.scope(), request.id()), Ok(state));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cancellation {
    Request,
    Approval,
    Session,
}

const CANCELLATIONS: [Cancellation; 3] = [
    Cancellation::Request,
    Cancellation::Approval,
    Cancellation::Session,
];

impl Cancellation {
    fn apply(self, core: &mut ControlPlane, request: &ToolRequest) -> Result<(), Error> {
        match self {
            Self::Request => core.cancel_request(request.scope(), request.id()),
            Self::Approval => core.revoke_approval(request.scope(), request.id()),
            Self::Session => core.cancel_session(request.scope()),
        }
    }

    fn queued_state(self) -> RequestState {
        match self {
            Self::Approval => RequestState::Revoked,
            Self::Request | Self::Session => RequestState::Cancelled,
        }
    }

    fn denied(self, state: RequestState) -> Error {
        if self == Self::Session {
            Error::SessionCancelled
        } else {
            Error::InvalidState(state)
        }
    }
}

#[test]
fn approval_binding_survives_public_api_to_gateway_composition() {
    let original = request(scope("tenant-1", "session-1"));
    let mut core = approved(&original);
    core.create_session(scope("tenant-2", "session-1")).unwrap();
    core.create_session(scope("tenant-1", "session-2")).unwrap();

    // Three distinct storage keys and six substitutions of the same key.
    for case in 0..9 {
        let mut other_scope = original.scope().clone();
        let mut id = original.id().clone();
        let mut identity = original.identity().clone();
        let mut tool = original.tool();
        let mut target = original.target();
        let mut arguments = original.arguments();
        let mut effect = original.effect();
        match case {
            0 => other_scope = scope("tenant-2", "session-1"),
            1 => other_scope = scope("tenant-1", "session-2"),
            2 => id = RequestId::new("request-2").unwrap(),
            3 => {
                identity = Identity::Machine {
                    service: PrincipalId::new("user-1").unwrap(),
                };
            }
            4 => {
                identity = Identity::OnBehalfOf {
                    subject: PrincipalId::new("user-2").unwrap(),
                };
            }
            5 => tool = "other.write",
            6 => target = "other-target",
            7 => arguments = br#"{"value": 1}"#,
            8 => effect = EffectKind::ReadOnly,
            _ => unreachable!(),
        }
        let changed =
            ToolRequest::new(other_scope, id, identity, tool, target, arguments, effect).unwrap();
        let denied = if case < 3 {
            Error::UnknownRequest
        } else {
            Error::RequestMismatch
        };
        assert_eq!(core.begin(&changed).unwrap_err(), denied, "case {case}");
        assert_eq!(
            core.approve(&changed, approver(&changed)),
            Err(denied),
            "case {case}"
        );
        assert_eq!(
            core.state(original.scope(), original.id()),
            Ok(RequestState::Approved)
        );
    }
    let approval = core
        .approval(original.scope(), original.id())
        .unwrap()
        .unwrap();
    assert_eq!(approval.request(), &original);
    assert_eq!(approval.approver(), &approver(&original));
    let permit = core.begin(&original).unwrap();
    assert_eq!(permit.request(), &original);
    dispatch(&permit);
    core.finish(&permit, Outcome::Failed).unwrap();
    assert_terminal(&mut core, &original, Some(&permit), RequestState::Failed);
}

#[test]
fn unavailable_dispatch_is_a_confirmed_terminal_local_failure() {
    for identity in [
        Identity::OnBehalfOf {
            subject: PrincipalId::new("user-1").unwrap(),
        },
        Identity::Machine {
            service: PrincipalId::new("service-1").unwrap(),
        },
    ] {
        for effect in [EffectKind::ReadOnly, EffectKind::ExternalWrite] {
            let base = request(scope("tenant-1", "session-1"));
            let request = ToolRequest::new(
                base.scope().clone(),
                base.id().clone(),
                identity.clone(),
                base.tool(),
                base.target(),
                base.arguments(),
                effect,
            )
            .unwrap();
            let mut core = approved(&request);
            let permit = core.begin(&request).unwrap();
            dispatch(&permit);
            core.finish(&permit, Outcome::Failed).unwrap();
            assert_terminal(&mut core, &request, Some(&permit), RequestState::Failed);
        }
    }
}

#[test]
fn cancelled_and_revoked_requests_remain_terminal() {
    for cancellation in [Cancellation::Request, Cancellation::Approval] {
        for already_approved in [false, true] {
            let request = request(scope("tenant-1", "session-1"));
            let mut core = ControlPlane::new();
            core.create_session(request.scope().clone()).unwrap();
            core.submit(request.clone()).unwrap();
            if already_approved {
                core.approve(&request, approver(&request)).unwrap();
            }
            cancellation.apply(&mut core, &request).unwrap();
            assert_terminal(&mut core, &request, None, cancellation.queued_state());
        }
    }
}

#[test]
fn sixteen_contenders_consume_approval_and_finish_only_once() {
    let request = request(scope("tenant-1", "session-1"));
    let core = Mutex::new(approved(&request));
    let barrier = Barrier::new(CONTENDERS);
    let mut permits = thread::scope(|threads| {
        let handles: Vec<_> = (0..CONTENDERS)
            .map(|_| {
                threads.spawn(|| {
                    barrier.wait();
                    core.lock().unwrap().begin(&request)
                })
            })
            .collect();
        let mut permits = Vec::new();
        for handle in handles {
            match handle.join().unwrap() {
                Ok(permit) => permits.push(permit),
                Err(error) => assert_eq!(error, Error::InvalidState(RequestState::Executing)),
            }
        }
        permits
    });
    assert_eq!(permits.len(), 1);
    let permit = permits.pop().unwrap();
    thread::scope(|threads| {
        for _ in 0..CONTENDERS {
            threads.spawn(|| dispatch(&permit));
        }
    });
    let results = thread::scope(|threads| {
        let handles: Vec<_> = (0..CONTENDERS)
            .map(|_| {
                threads.spawn(|| {
                    barrier.wait();
                    core.lock().unwrap().finish(&permit, Outcome::Failed)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for error in results.into_iter().filter_map(Result::err) {
        assert_eq!(error, Error::InvalidState(RequestState::Failed));
    }
    assert_terminal(
        &mut core.into_inner().unwrap(),
        &request,
        Some(&permit),
        RequestState::Failed,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CancelAt {
    BeforeBegin,
    BeforeDispatch,
    BeforeFinish,
    AfterFinish,
}

fn ordered_cancellation(cancellation: Cancellation, at: CancelAt) {
    let request = request(scope("tenant-1", "session-1"));
    let core = Mutex::new(approved(&request));
    let (arrived, cancel_now) = mpsc::sync_channel(0);
    let (cancelled, resume) = mpsc::sync_channel(0);
    let permit = thread::scope(|threads| {
        let core = &core;
        let request = &request;
        let worker = threads.spawn(move || {
            let pause = || {
                arrived.send(()).unwrap();
                resume.recv().unwrap();
            };
            if at == CancelAt::BeforeBegin {
                pause();
                assert_eq!(
                    core.lock().unwrap().begin(request).unwrap_err(),
                    cancellation.denied(cancellation.queued_state())
                );
                return None;
            }
            let permit = core.lock().unwrap().begin(request).unwrap();
            if at == CancelAt::BeforeDispatch {
                pause();
            }
            dispatch(&permit);
            if at == CancelAt::BeforeFinish {
                pause();
            }
            let finished = core.lock().unwrap().finish(&permit, Outcome::Failed);
            if at == CancelAt::AfterFinish {
                assert_eq!(finished, Ok(()));
                pause();
            } else {
                assert_eq!(
                    finished,
                    Err(cancellation.denied(RequestState::OutcomeUnknown))
                );
            }
            Some(permit)
        });
        let canceller = threads.spawn(move || {
            cancel_now.recv().unwrap();
            let result = cancellation.apply(&mut core.lock().unwrap(), request);
            if at == CancelAt::AfterFinish && cancellation != Cancellation::Session {
                assert_eq!(result, Err(Error::InvalidState(RequestState::Failed)));
            } else {
                assert_eq!(result, Ok(()));
            }
            cancelled.send(()).unwrap();
        });
        let permit = worker.join().unwrap();
        canceller.join().unwrap();
        permit
    });
    let expected = match at {
        CancelAt::BeforeBegin => cancellation.queued_state(),
        CancelAt::AfterFinish => RequestState::Failed,
        CancelAt::BeforeDispatch | CancelAt::BeforeFinish => RequestState::OutcomeUnknown,
    };
    assert_terminal(
        &mut core.into_inner().unwrap(),
        &request,
        permit.as_ref(),
        expected,
    );
}

#[test]
fn cancellation_before_begin_never_produces_a_dispatch_permit() {
    for cancellation in CANCELLATIONS {
        ordered_cancellation(cancellation, CancelAt::BeforeBegin);
    }
}

#[test]
fn dispatch_cancellation_windows_preserve_the_exact_recorded_outcome() {
    for cancellation in CANCELLATIONS {
        for at in [
            CancelAt::BeforeDispatch,
            CancelAt::BeforeFinish,
            CancelAt::AfterFinish,
        ] {
            ordered_cancellation(cancellation, at);
        }
    }
}

#[test]
fn simultaneous_begin_and_cancellation_have_only_legal_results() {
    for cancellation in CANCELLATIONS {
        for _ in 0..RACE_ITERATIONS {
            let request = request(scope("tenant-1", "session-1"));
            let core = Mutex::new(approved(&request));
            let barrier = Barrier::new(2);
            let begun = thread::scope(|threads| {
                let worker = threads.spawn(|| {
                    barrier.wait();
                    let begun = core.lock().unwrap().begin(&request);
                    if let Ok(permit) = &begun {
                        dispatch(permit);
                    }
                    begun
                });
                let canceller = threads.spawn(|| {
                    barrier.wait();
                    cancellation.apply(&mut core.lock().unwrap(), &request)
                });
                let begun = worker.join().unwrap();
                assert_eq!(canceller.join().unwrap(), Ok(()));
                begun
            });
            let (permit, expected) = match begun {
                Ok(permit) => (Some(permit), RequestState::OutcomeUnknown),
                Err(error) => {
                    let expected = cancellation.queued_state();
                    assert_eq!(error, cancellation.denied(expected));
                    (None, expected)
                }
            };
            assert_terminal(
                &mut core.into_inner().unwrap(),
                &request,
                permit.as_ref(),
                expected,
            );
        }
    }
}

#[test]
fn simultaneous_finish_and_cancellation_have_only_legal_results() {
    for cancellation in CANCELLATIONS {
        for _ in 0..RACE_ITERATIONS {
            let request = request(scope("tenant-1", "session-1"));
            let mut core = approved(&request);
            let permit = core.begin(&request).unwrap();
            dispatch(&permit);
            let core = Mutex::new(core);
            let barrier = Barrier::new(2);
            let (finished, cancelled) = thread::scope(|threads| {
                let worker = threads.spawn(|| {
                    barrier.wait();
                    core.lock().unwrap().finish(&permit, Outcome::Failed)
                });
                let canceller = threads.spawn(|| {
                    barrier.wait();
                    cancellation.apply(&mut core.lock().unwrap(), &request)
                });
                (worker.join().unwrap(), canceller.join().unwrap())
            });
            let expected = if finished.is_ok() {
                assert_eq!(
                    cancelled,
                    if cancellation == Cancellation::Session {
                        Ok(())
                    } else {
                        Err(Error::InvalidState(RequestState::Failed))
                    }
                );
                RequestState::Failed
            } else {
                assert_eq!(cancelled, Ok(()));
                assert_eq!(
                    finished,
                    Err(cancellation.denied(RequestState::OutcomeUnknown))
                );
                RequestState::OutcomeUnknown
            };
            assert_terminal(
                &mut core.into_inner().unwrap(),
                &request,
                Some(&permit),
                expected,
            );
        }
    }
}
