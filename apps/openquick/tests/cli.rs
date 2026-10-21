use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_openquick"))
        .args(args)
        .output()
        .expect("run local CLI")
}

#[test]
fn status_discloses_all_unavailable_integrations() {
    let output = run(&["status"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for component in [
        "AgentGateway",
        "IdP / credential broker",
        "MicroVM workers",
        "BI / QuickSight compatibility",
        "durable recovery",
    ] {
        assert!(stdout.contains(&format!("{component}: unavailable")));
    }
}

#[test]
fn gateway_fails_without_claiming_dispatch() {
    let output = run(&["gateway"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("no tool was dispatched")
    );
}

#[test]
fn unsupported_arguments_do_not_run_work() {
    for args in [vec!["unknown"], vec!["status", "ignored"]] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn help_lists_supported_commands() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("status|gateway")
    );
}

#[test]
fn demo_exercises_core_and_gateway_without_external_work() {
    let output = run(&["demo"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for evidence in [
        "Synthetic local demo; no network requests or external writes.",
        "missing approval: denied",
        "no tool was dispatched",
        "simulated unknown outcome: retry denied",
        "cancelled / revoked requests: admission denied",
        "external security acceptance remains unavailable",
    ] {
        assert!(
            stdout.contains(evidence),
            "missing demo evidence: {evidence}"
        );
    }
}
