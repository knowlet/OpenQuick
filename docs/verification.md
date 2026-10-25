# Independent review and red/green evidence

Environment: Debian 13.6, x86_64-unknown-linux-gnu, Rust/Cargo 1.90.0. Working tree source was copied into `/tmp/openquick-review-sufgsyjn` with a core-only workspace manifest; no original repo source was mutated. Source was not yet committed at the time of this test. Main workspace checks remain the parent agent's responsibility.

1. Baseline `cargo test --manifest-path /tmp/openquick-review-sufgsyjn/Cargo.toml -p openquick-core --locked`: exit 0, 12 integration tests passed.
2. In the isolated copy only, removed the exact `if entry.request != *request { return Err(Error::RequestMismatch); }` guard from `ControlPlane::expected_entry_mut`.
3. Red `cargo test --manifest-path /tmp/openquick-review-sufgsyjn/Cargo.toml -p openquick-core --test control_plane --locked approval_and_begin_reject_every_request_substitution -- --exact`: exit 101. The test failed on `left: Ok(())`, `right: Err(RequestMismatch)`, proving a substituted request was accepted without the guard.
4. Restored the copied source. Same exact regression command: exit 0; 1 passed, 11 filtered out.

Review coverage: full immutable request binding (including identity mode, tool, target, argument bytes and effect); tenant/session separation; one execution per request; terminal cancellation/revocation/known results/unknown outcomes; in-flight cancellation preserves uncertainty; ControlPlane issuer marker prevents cross-instance permit use. No must-fix correctness issue observed in core at this stage.

Limits: this tests in-memory trusted-caller behavior only. No authentication/authorization provider, durable journal, crash recovery, external gateway behavior, MicroVM isolation, real hardware or BI capability was tested. A future adapter must recheck current state at the actual dispatch boundary and disable external-write retries. Different request IDs can still represent the same semantic operation; no semantic deduplication is provided.

Latest snapshot red/green repeated after all 12 core tests were written (including M2M allowed path, read-only unknown terminal, and same-mode OBO subject substitution). Baseline and restored regression exit 0; exact mutation regression exit 101.

Reviewed source SHA256 (before Git commit):
- `crates/core/src/lib.rs`: `0ed2d697004cf5a1a4722eb30e55c5ae8fda2164df2718752ef7cdc5b9bdd39a`
- `crates/core/tests/control_plane.rs`: `125622614509f8f3927cafbd013ddd65a359f9aff6731d1320794e76e58e1939`
- `crates/gateway/src/lib.rs`: `15fc0b77eb6d474d64ea08d5264b55e990e25b41eea617bb87d908be7678a15e`
- `apps/openquick/src/main.rs`: `2455a38cdb285edb14a7e59ca0ddb333ad0232cb9f1830bc8709b81e6ea1a4e0`
- `apps/openquick/tests/cli.rs`: `ded1f62d78c49d0ac9f72390cd4885a83facedd5f1c345aed3aba3315c64db9a`

CLI composition review: `demo` creates credential-free synthetic requests, exercises explicit approval, unavailable dispatch, unknown no-retry, cancellation and revocation. A disabled dispatch is recorded as a confirmed local failure; synthetic unknown is clearly labeled separately. `gateway` returns exit 1 and claims no dispatch; unsupported arguments exit 2 without work. Status discloses all unavailable integrations. No must-fix CLI correctness issue observed.

## Additional terminal-state acceptance (2026-10-22)

Reviewed immutable PR head `a7b31a4420883a51b3874b36357ad483e6e895e5` in a separate source snapshot. No implementation defect was found; this update adds two acceptance tests without changing runtime behavior:

- Every completed outcome rejects approval, another begin, cancellation, revocation, repeated finish with any outcome, and reuse of the request ID; the recorded result stays unchanged.
- A session containing all eight request states cancels queued requests, makes executing requests uncertain, preserves terminal results across repeated cancellation, and leaves both another tenant's same-named session and a sibling session executable.

The available local cached toolchain was Rust/Cargo **1.99.1**, not the repository-pinned 1.90.0. Commands deliberately used `cargo +stable`; the pin was not changed. Baseline: 12 core integration tests, 5 CLI tests, 1 gateway test passed. Updated: 14 core integration tests, 5 CLI tests, 1 gateway test passed. `cargo +stable fmt --all -- --check`, `cargo +stable clippy --workspace --all-targets --locked -- -D warnings`, `cargo +stable test --workspace --locked`, and `cargo +stable build --workspace --release --locked` each exited 0. These local results do not establish pinned-toolchain or remote-CI success.

Two isolated mutation checks demonstrated that the new assertions detect meaningful regressions. In each case, only a disposable source copy was mutated; the submitted implementation stayed unchanged:

1. Allowing `cancel_request` to cancel a `Failed` request caused `every_terminal_outcome_rejects_all_lifecycle_mutations` to fail (exit 101, `Ok(())` rather than `Err(InvalidState(Failed))`). Restoring the implementation made the same exact test pass (exit 0).
2. Replacing the terminal-state preservation arm in `cancel_session` with `Cancelled` caused `session_cancellation_preserves_terminal_results_and_other_scopes` to fail (exit 101, `Cancelled` rather than `Succeeded`). Restoring the implementation made the same exact test pass (exit 0).

For each mutation the command was `cargo +stable test -p openquick-core --test control_plane --locked TEST_NAME -- --exact`. This is synthetic trusted-library acceptance only; durable recovery, authentication, actual dispatch and external effects remain unimplemented and untested.

Independent review subsequently made the pinned Rust/Cargo **1.90.0** toolchain available and verified the updated snapshot with `cargo +1.90.0 test --workspace --locked` (14 core integration tests, 5 CLI tests, 1 gateway test), `cargo +1.90.0 fmt --all -- --check`, `cargo +1.90.0 clippy --workspace --all-targets --locked -- -D warnings`, and `cargo +1.90.0 build --workspace --release --locked`; all exited 0. This establishes local pinned-toolchain success for the updated snapshot, not remote-CI success. The earlier 1.99.1 baseline and mutation results remain separate evidence.

## Offline composition and concurrency (2026-10-25)

Starting remote PR #1 head: `b796b1275db1157c33eb0d772a0c512b496805b6`; base `develop`: `e464a6ef01ca171a31265f0a26da5ea1918f249f`. The clean initial checkout was on `work` at the base. Work used an isolated `test/control-plane-offline-validation` worktree. Neither checkout nor `/workspace/.agents` contained `AGENTS.md` or `.agents/skills`. Existing user files were preserved. PR #1 remains Draft.

Environment: Codex Cloud, Debian 13.6, Linux 6.18.55, x86_64; Rust `1.90.0 (1159e78c4 2025-09-14)`, Cargo `1.90.0 (840b83a10 2025-07-30)`, Python 3.12.14 and strace 6.13. The official Rust 1.90.0 distribution was installed in the workspace with rustfmt/Clippy after its SHA256 matched `bff8974f2d3ee6c0e6ac926b533f65bbdd3697d2c2b925bdae5f45b9eed10a67`. The repository toolchain pin and lockfile were unchanged. All following Cargo checks used this toolchain and `--offline` where supported; toolchain acquisition and GitHub operations are separate network operations.

No runtime defect was found. Runtime sources, dependencies and CI configuration are unchanged. The existing [fixed-revision source research](research/first-slice.md) was reused: AgentGateway remains external and unavailable; no upstream code was copied, no package was added, and no persistence or transport design was introduced.

`apps/openquick/tests/offline_control_plane.rs` adds eight integration tests calling public core APIs and the actual `Gateway.dispatch`, without a mock gateway:

| Coverage | Executed cases |
| --- | --- |
| Approval binding | Nine substitutions: tenant, session, request ID, OBO/Machine mode, OBO subject, tool, target, exact argument whitespace and effect; the original approved request still reaches the closed gateway |
| IntegrationUnavailable | Four OBO/Machine × read/write paths record confirmed local `Failed` and reject subsequent lifecycle operations |
| Queued terminal states | Pending/approved × cancel/revoke stay terminal on repeated operations |
| Concurrent permit/result use | 16 simultaneous begins yield exactly one permit; 16 calls with that retained permit all reach the real unavailable gateway; 16 simultaneous finishes yield exactly one success |
| Deterministic cancellation windows | Request cancel, approval revoke and session cancel × before begin, before dispatch, before finish and after finish: 12 channel-coordinated cases, without sleeps |
| Begin/cancel race | Three cancellation kinds × 32 barrier-started iterations = 96; either no permit or terminal uncertainty |
| Finish/cancel race | Three cancellation kinds × 32 barrier-started iterations = 96; either confirmed failure or terminal uncertainty, never overwritten |

The core is single-owner and requires `&mut self`. Tests supply a caller-owned `Mutex`; they do not prove distributed coordination. `Gateway.dispatch` always rejects, even with a retained/stale permit. This does not implement live cancellation revalidation at a real transport boundary. A permit can be presented repeatedly to this closed gateway, but can produce neither a second core execution nor an external dispatch.

### Commands and results

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Exit 0 |
| `cargo test --workspace --locked --offline` | Exit 0; 28 tests: 14 core + 8 composition + 5 CLI + 1 gateway |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Exit 0 |
| `cargo build --workspace --release --locked --offline` | Exit 0 |
| `cargo test -p openquick --test cli --locked --offline` | Exit 0; 5 tests |
| `cargo test -p openquick --test offline_control_plane --locked --offline` repeated 20 times | 20/20 exit 0; 160 test executions, including 3,840 race iterations |
| `cargo tree --locked --offline` | Exit 0; only the three local packages |
| `python3 scripts/verify_offline.py --e2e-binary EXECUTABLE --output-dir FRESH_DIRECTORY` | Exit 0; 5 CLI paths (326 syscalls) and all 8 integration tests (8,346 syscalls), zero target violations or working-directory changes |
| `git diff --check` | Exit 0 |

Baseline before adding tests: the same pinned-toolchain fmt, tests (20), Clippy and release build passed. No baseline failure was hidden by the added tests.

### Observing actual offline effects

`scripts/verify_offline.py` uses Python's standard library and the installed Linux `strace`. It traces **all** syscalls of the actual release `status`, `demo`, `gateway`, unknown-command and extra-argument paths, with expected exits 0/0/1/2/2. It can also trace the integration-test executable. It rejects network calls, file mutations, writes outside stdout/stderr, shared mappings, child processes, unparsed/unknown calls, missing or repeated initial execution, timeouts and unexpected exit/output. The integration suite may create threads with `CLONE_THREAD`. A temporary working-directory snapshot supplies an additional mutation check.

The observer includes two actual local negative controls: a shell writes a temporary sentinel, and a shell starts an extra process. Both must execute and be detected as violations. These controls and observer artifacts are the verifier's own intentional local effects; the target CLI/test runs must have zero violations. This is observation after execution, not a sandbox or a guarantee for arbitrary future code.

Reproduce with a fresh output directory:

```sh
cargo build --workspace --release --locked --offline
cargo test -p openquick --test offline_control_plane --no-run --locked --offline --message-format=json > /tmp/openquick-test-artifacts.jsonl
e2e_binary=$(python3 - <<'PY'
import json
from pathlib import Path
rows = map(json.loads, Path('/tmp/openquick-test-artifacts.jsonl').read_text().splitlines())
print(next(row['executable'] for row in rows
           if row.get('reason') == 'compiler-artifact'
           and row['target']['name'] == 'offline_control_plane'
           and row.get('executable')))
PY
)
python3 scripts/verify_offline.py --e2e-binary "$e2e_binary" --output-dir /tmp/openquick-offline-evidence
```

`--no-run` above only obtains the current test executable path; the observer then executes all eight tests. No credentials, service endpoint or paid API is used. Per-thread traces, stdout/stderr, binary fingerprints and `summary.json` are retained in the output directory. The observer is a local acceptance check; the existing GitHub Rust workflow automatically runs the new integration tests, but does not run the syscall observer.

Observed exits were exactly 0/0/1/2/2 for the five CLI paths and 0 for the integration binary. Both negative controls executed with exit 0 and were correctly rejected (six violations each). Independent review found that checking only `test result: ok.` could accept a zero-test suite; the observer now requires all eight named tests and zero failed/ignored/filtered tests. The actual zero-test core unit-test executable was rejected (observer exit 1), while the complete integration suite passed (exit 0). Syscall counts are an observed sample and can vary with scheduling/runtime; acceptance depends on behavior, not an exact count. Evidence in this Cloud workspace is under `/workspace/openquick-validation-evidence/` (`checks.json`, `mutations.json`, stress logs, `zero-suite-final/summary.json` and `observation-final-complete/summary.json`).

Source SHA256 fingerprints for this verification snapshot:

- `apps/openquick/tests/offline_control_plane.rs`: `35af542aa28faed1d2d59902967ee943e678666decba0133ebd21b6dfce93871`
- `scripts/verify_offline.py`: `0a70f77f7fb2d5840150aa7db2d8d5996feed1dbbc2a1871682d265bdb6044f6`

The three runtime-source fingerprints remain identical to the initial reviewed fingerprints above.

### Mutation controls

Only an independent disposable copy was modified. Each selected new integration test passed before mutation (exit 0), failed through a test assertion after mutation (exit 101), then passed after restoration (exit 0):

| Disposable mutation | Exact test selector |
| --- | --- |
| Remove full-request equality guard | `approval_binding_survives_public_api_to_gateway_composition` |
| Allow another begin in `Executing` | `sixteen_contenders_consume_approval_and_finish_only_once` |
| Closed gateway incorrectly returns `Ok(())` (still no transport) | `unavailable_dispatch_is_a_confirmed_terminal_local_failure` |
| In-flight request cancellation records `Cancelled` instead of uncertainty | `dispatch_cancellation_windows_preserve_the_exact_recorded_outcome` |

Command for each phase: `cargo test -p openquick --test offline_control_plane --locked --offline TEST_SELECTOR -- --exact`. No submitted runtime guard was weakened.

### Remaining acceptance limits

Durable journal/restart recovery, real AgentGateway dispatch, transport-time cancellation/revocation coordination, authenticated approvers/OBO/M2M, credentials, worker isolation, semantic duplicate detection and remote reconciliation remain unimplemented. No real external action, power-loss/restart test, paid API, deployment, release or merge was performed. The project license remains an owner decision. The resulting remote head and exact-associated CI checkout evidence belong in PR #1 and the delivery report; historical green CI is not evidence for a later head.
