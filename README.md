# OpenQuick

OpenQuick is a new Rust monorepo for an independent implementation informed by public Amazon Quick and QuickSight documentation. The long-term goal is an open-source system for connected research, automation, and BI. The first slice is a local control-plane library and a synthetic CLI demonstration.

This is newly written work. No inaccessible durable-core ZIP or scaffold from another environment was read or imported. Public AWS documentation supplies product requirements; no Amazon proprietary implementation was consulted. Open-source infrastructure source was reviewed as recorded in the [research notes](docs/research/first-slice.md).

## Current scope

| Component | Available in this slice | Boundary |
| --- | --- | --- |
| `crates/core` | In-memory tenant/session identity, immutable tool requests, explicit approval binding, and testable execution state transitions | Trusted library callers only; no authentication service or durable storage |
| `crates/gateway` | An explicit integration-unavailable error | AgentGateway remains an external component; no proxy protocol or network dispatch is implemented |
| `apps/openquick` | Local status and synthetic control-plane demo; unavailable gateway command exits unsuccessfully | No model, credentials, network requests, or real tools |

The core denies execution without approval or after cancellation/revocation and keeps unknown outcomes terminal. Approval binds the entire stored request, including its tenant, session, request ID, identity, tool, target, argument bytes and effect kind. Both read and external-write requests require approval and have terminal outcomes without automatic retries. OBO and machine identity are separate data variants; neither authenticates a caller.

AgentGateway, IdP/OAuth, per-session MicroVM workers, credential brokerage, connectors, BI, dashboards and Amazon API compatibility are **unavailable**. Tests of local state transitions do not establish those integrations or production security. See [architecture and limits](docs/architecture.md) and the [acceptance matrix](docs/acceptance.md).

## Try locally

Requires Git and Rust 1.90.0 with `rustfmt` and `clippy`, pinned in `rust-toolchain.toml`. From this checkout:

```sh
cargo run -p openquick --locked -- status
cargo run -p openquick --locked -- demo
cargo run -p openquick --locked -- gateway
```

`status` reports availability. `demo` exercises synthetic local state transitions and performs no external action. `gateway` reports that AgentGateway integration is unavailable and returns a nonzero exit status; this is the expected result.

The workspace has no registry dependencies. A first build may still require downloading the pinned Rust toolchain. No cloud service or paid API is needed for the local commands.

## Develop and verify

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
cargo test -p openquick --test cli --locked
```

The Rust CI runs these checks on Ubuntu 24.04 with read-only repository permission, fixed action revisions, and a time limit. Exact commit and run results belong in the [verification record](docs/acceptance.md); a workflow file alone is not evidence that CI passed. Other operating systems and actual isolation hardware are not certified by this slice.

## License and provenance

The project license is **undecided**. No Apache, MIT, or other project license grant is implied. Workspace packages set `publish = false`; choosing a license remains an owner decision. [Third-party evidence](docs/third-party.md) describes dependency/tool licenses separately and does not assign their licenses to OpenQuick.
