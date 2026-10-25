use openquick_core::{
    Approver, ControlPlane, EffectKind, Error, Identity, Outcome, PrincipalId, RequestId,
    RequestState, SessionId, SessionScope, TenantId, ToolRequest,
};
use openquick_gateway::Gateway;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            print_help();
            ExitCode::SUCCESS
        }
        [command] if command == "--help" || command == "help" => {
            print_help();
            ExitCode::SUCCESS
        }
        [command] if command == "status" => {
            println!("core: local in-memory control-plane model");
            println!("AgentGateway: unavailable");
            println!("IdP / credential broker: unavailable");
            println!("MicroVM workers: unavailable");
            println!("BI / QuickSight compatibility: unavailable");
            println!("durable recovery: unavailable");
            ExitCode::SUCCESS
        }
        [command] if command == "gateway" => {
            match Gateway.availability() {
                Err(error) => eprintln!("{error}"),
                Ok(()) => eprintln!("unexpected gateway availability"),
            }
            ExitCode::FAILURE
        }
        [command] if command == "demo" => match demo() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("local demo failed: {error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("unsupported arguments; run openquick --help");
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("OpenQuick: local control-plane verification slice");
    println!("Usage: openquick [status|gateway|demo|--help]");
    println!("No external integrations are enabled.");
}

fn demo() -> Result<(), Box<dyn std::error::Error>> {
    println!("Synthetic local demo; no network requests or external writes.");
    let scope = SessionScope::new(
        TenantId::new("demo-tenant")?,
        SessionId::new("demo-session")?,
    );
    let mut core = ControlPlane::new();
    core.create_session(scope.clone())?;
    let make_request = |id: &str| {
        ToolRequest::new(
            scope.clone(),
            RequestId::new(id)?,
            Identity::OnBehalfOf {
                subject: PrincipalId::new("demo-user")?,
            },
            "example.write",
            "local-demo",
            br#"{"value":1}"#.to_vec(),
            EffectKind::ExternalWrite,
        )
    };
    let approver = Approver::new(scope.clone(), PrincipalId::new("demo-reviewer")?);
    let request = make_request("unavailable-dispatch")?;
    core.submit(request.clone())?;
    require(
        matches!(core.begin(&request), Err(Error::ApprovalRequired)),
        "unapproved admission was allowed",
    )?;
    println!("missing approval: denied");
    core.approve(&request, approver.clone())?;
    let permit = core.begin(&request)?;
    let unavailable = Gateway
        .dispatch(&permit)
        .expect_err("gateway has no transport");
    println!("{unavailable}");
    // The disabled adapter proves no dispatch occurred. This is a confirmed
    // local failure, separate from the synthetic unknown-result example below.
    core.finish(&permit, Outcome::Failed)?;

    let request = make_request("simulated-unknown")?;
    core.submit(request.clone())?;
    core.approve(&request, approver.clone())?;
    let permit = core.begin(&request)?;
    core.finish(&permit, Outcome::Unknown)?;
    require(
        matches!(
            core.begin(&request),
            Err(Error::InvalidState(RequestState::OutcomeUnknown))
        ),
        "unknown outcome was allowed to retry",
    )?;
    println!("simulated unknown outcome: retry denied");

    for (id, revoke) in [("cancelled", false), ("revoked", true)] {
        let request = make_request(id)?;
        core.submit(request.clone())?;
        core.approve(&request, approver.clone())?;
        if revoke {
            core.revoke_approval(&scope, request.id())?;
        } else {
            core.cancel_request(&scope, request.id())?;
        }
        require(
            core.begin(&request).is_err(),
            "terminal request was admitted",
        )?;
    }
    println!("cancelled / revoked requests: admission denied");
    println!("Local state checks passed; external security acceptance remains unavailable.");
    Ok(())
}

fn require(condition: bool, failure: &'static str) -> Result<(), Box<dyn std::error::Error>> {
    if condition {
        Ok(())
    } else {
        Err(failure.into())
    }
}
