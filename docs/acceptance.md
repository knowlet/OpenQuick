# Acceptance and verification

This document separates local control-plane acceptance from unavailable product and infrastructure integrations. It records the first in-memory slice; it does not claim production security, durable execution or Amazon API compatibility.

## Local acceptance matrix

The following cases define the executable acceptance surface. All local cases passed on 2026-10-21; commands, test counts and the independently reviewed source fingerprints are recorded below and in the [verification evidence](verification.md).

| Case | Required observable behavior |
| --- | --- |
| Identifiers and session ownership | Invalid identifiers fail; sessions are keyed by tenant/session scope; request identity remains part of approval binding |
| Approval required | A pending tool request cannot begin before explicit approval |
| Full approval binding | A changed tenant, session, identity, request ID, tool, target, argument bytes or effect kind cannot reuse approval |
| Single execution | Beginning consumes approval; a repeated begin cannot produce another execution |
| Request cancellation | Cancelled work cannot begin |
| Approval revocation | Revoked work cannot begin |
| Session cancellation | Cancelled sessions reject new work and prevent eligible existing work from beginning |
| In-flight cancellation/revocation | State preserves an unknown result instead of asserting a remote action was undone |
| Unknown external write result | Unknown remains terminal and cannot automatically retry the same request |
| Permit origin | A permit issued by another control-plane instance is rejected |
| Terminal result | A completed request cannot be executed again using its previous approval |
| Unavailable gateway | Availability and dispatch return an explicit integration-unavailable error |
| CLI smoke | `status` and `demo` complete locally; `gateway` returns nonzero with an unavailable message; invalid commands fail |

Tests must cover both allowed and rejected transitions, including cross-tenant/session substitutions. The core is a trusted library; these cases do not authenticate an attacker or validate a network authorization boundary.

## Reproduce checks

The toolchain is pinned to Rust 1.90.0, including `rustfmt` and `clippy`.

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
cargo test -p openquick --test cli --locked
```

CLI manual smoke:

```sh
cargo run -p openquick --locked -- status
cargo run -p openquick --locked -- demo
cargo run -p openquick --locked -- gateway
```

The last command must return nonzero. No command should contact a model, an IdP, AgentGateway or a real tool.

## Verification record

Local execution environment: Debian 13.6, Linux 6.18.55, `x86_64-unknown-linux-gnu`, Rust/Cargo 1.90.0. The existing checkout began on unborn `work`, with no user files, `AGENTS.md` or repository skills. The initial `develop` README commit is `e464a6ef01ca171a31265f0a26da5ea1918f249f`; implementation work uses `feat/control-plane-foundation`.

| Check | Observed result |
| --- | --- |
| `cargo fmt --all -- --check` | Exit 0 |
| `cargo test --workspace --locked` | Exit 0; 12 core integration tests, 5 CLI smoke tests, 1 gateway unit test; no failures |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Exit 0 |
| `cargo build --workspace --release --locked` | Exit 0 |
| `cargo test -p openquick --test cli --locked` | Exit 0; all 5 passed |
| Release CLI `status` / `demo` | Exit 0; unavailable integrations disclosed / synthetic checks passed |
| Release CLI `gateway` | Expected exit 1; `no tool was dispatched` |
| `cargo tree --locked` | Only three local workspace packages; no external Cargo packages |
| `git diff --check` | Exit 0 |

The approval-substitution regression was demonstrated in a disposable copy of the final 12-test core: baseline exit 0; removal of the full-request comparison guard made the exact regression fail with exit 101 (`Ok(())` instead of `Err(RequestMismatch)`); restoring the guard made it pass with exit 0. No working-tree guard or test was weakened. See [commands and source SHA256 fingerprints](verification.md).

These local results describe the implementation files bundled with this record, checked before publication. The resulting Git SHA, remote-head verification and exact-head CI run links are recorded in the Draft PR and delivery report. A workflow file alone is not CI evidence. Pull-request merge-ref runs must be distinguished from branch-head runs; a different commit's green run is not evidence for the current head.

## Unavailable and unverified acceptance

| Area | Current limitation / future evidence required |
| --- | --- |
| Durable execution | In-memory only; restart loses state. No journal, recovery, crash injection or power-loss test is implemented |
| Authenticated identity | OBO and Machine are data variants only. No IdP/token/consent/authentication integration |
| Approver authorization | Trusted library callers provide approvals. No verified human identity, approval UI or role policy |
| AgentGateway | No process launch, network connection, proxy routing, credential brokerage or retry-policy integration |
| External actions | No real read/write tool is dispatched. No remote exactly-once or reconciliation guarantee |
| Cancellation after dispatch | A local cancellation cannot undo a remote effect. Future dispatch must revalidate current state and coordinate races |
| Semantic duplicates | Different request IDs may describe the same action; this slice does not detect or prevent those resubmissions |
| Worker isolation | No MicroVM runtime, guest image, per-session lifecycle, egress policy or hardware isolation test |
| Credentials | No credential API or store. No live secret-handling or model-boundary integration has been tested |
| BI and product parity | No data source, dataset engine, dashboard, research system or Quick/QuickSight compatible API |
| Platform coverage | CI targets Ubuntu 24.04. Other hosts, production infrastructure and physical hardware remain unverified |

## Next integration gate

Before enabling external writes, add durable intent/outcome storage and restart tests; authenticated user/service context and approver authorization; a real AgentGateway adapter with explicit write retry policy; and cancellation/revocation checks at the actual dispatch boundary. Test credential handling and isolation independently. These are remaining work, not security properties supplied by the current unavailable gateway.
