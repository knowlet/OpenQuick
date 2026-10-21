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
