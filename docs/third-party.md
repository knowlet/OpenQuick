# Third-party and project licensing evidence

OpenQuick's project license is **undecided**. No license grant is implied. Every workspace package sets `publish = false`; this build setting is not a license. The owner must choose project terms separately from the licenses of tools or future dependencies.

## Cargo dependencies in this slice

The workspace contains `openquick-core`, `openquick-gateway` and `openquick`. Dependencies between these packages use local paths. The first slice uses Rust's standard library and has no registry or Git package dependencies; no AgentGateway, Cedar or sled source is copied or vendored.

The Rust compiler and standard library are toolchain components with their own upstream terms, commonly MIT OR Apache-2.0. Their terms do not license OpenQuick. Official license sources are [rust-lang/rust LICENSE-MIT](https://github.com/rust-lang/rust/blob/1.90.0/LICENSE-MIT) and [LICENSE-APACHE](https://github.com/rust-lang/rust/blob/1.90.0/LICENSE-APACHE). Distributing a bundled toolchain or runtime artifact requires checking the components actually shipped and their notices.

## CI tooling

| Tool | Fixed revision | Observed upstream terms |
| --- | --- | --- |
| `actions/checkout` | `11d5960a326750d5838078e36cf38b85af677262` | [MIT](https://github.com/actions/checkout/blob/11d5960a326750d5838078e36cf38b85af677262/LICENSE) |
| `dtolnay/rust-toolchain` | `b725eda525362e7cf34af9a7243cac1e3f01a7ec` | [MIT-style license text](https://github.com/dtolnay/rust-toolchain/blob/b725eda525362e7cf34af9a7243cac1e3f01a7ec/LICENSE) |

The workflow invokes these tools; it does not copy their source into this repository. Both action revisions and license files were inspected on 2026-10-21. CI uses read-only repository permission and does not publish packages or deploy services.

## Referenced external projects

| Project | Use in OpenQuick | License evidence inspected |
| --- | --- | --- |
| AgentGateway | Future external component; not connected or bundled in this slice | [Apache-2.0](https://github.com/agentgateway/agentgateway/blob/f51a7fbca57dfe56bf639b8df21be49db76e9ebc/LICENSE) |
| Cedar | Authorization design comparison only; no dependency or copied code | [Apache-2.0](https://github.com/cedar-policy/cedar/blob/4d1ef853895d8a3fa8f3a6c9651b0eef21bf9edd/LICENSE) |
| sled | Durability design comparison only; no dependency or copied code | [MIT OR Apache-2.0 package declaration](https://github.com/spacejam/sled/blob/e449d17111f4a097e1c66b6db241962ccb6a4136/Cargo.toml), [Apache text](https://github.com/spacejam/sled/blob/e449d17111f4a097e1c66b6db241962ccb6a4136/LICENSE-APACHE) |

Root license inspection is not a complete audit of any upstream distribution. Before adding or distributing AgentGateway or other packages, verify the exact version, transitive dependencies, file-level exceptions, required notices and distribution method. No conclusion about compatibility with OpenQuick's future license is possible until that license is chosen.

## Public product references

Public Amazon Quick/QuickSight documentation informs requirements only. No proprietary code, unavailable durable-core archive, Amazon branding assets or undocumented implementation was imported. Product names identify comparison targets; no affiliation, API compatibility or feature equivalence is claimed. Source locations and actual files read are listed in the [research notes](research/first-slice.md).
