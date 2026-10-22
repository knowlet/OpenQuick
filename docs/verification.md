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
