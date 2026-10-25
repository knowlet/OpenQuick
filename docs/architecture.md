# Architecture: first control-plane slice

## Decision and provenance

OpenQuick separates a trusted control plane from gateway transport and untrusted agent execution. This slice is an original, dependency-light Rust implementation of control-plane data and transitions. No inaccessible archive was read or imported. Product requirements come from [public AWS documentation](research/first-slice.md#public-product-behavior-and-identity); this is not a compatibility or production-readiness claim.

AgentGateway will be integrated as an external open-source component. OpenQuick will not recreate its MCP/agent proxy protocols, handshakes, routing, or authentication implementation. Reading upstream infrastructure source informed the boundary decision; no upstream code was copied. See the fixed source revisions in the [research record](research/first-slice.md).

## Trust boundaries

```text
Trusted operator/application
          |
          v
    OpenQuick CLI
       /       \
trusted core   gateway integration boundary (unavailable)
                    |
             external AgentGateway (future)
                    |
               approved tools (future)

Future per-session MicroVM:
    untrusted agent worker -> constrained requests to trusted core
```

The trusted core and gateway stay outside the future worker MicroVM. Each untrusted worker session is intended to have its own MicroVM; this repository does not create, boot, connect, or isolate a worker. The diagram shows the target boundary, not deployed infrastructure.

`crates/core` contains the state machine. `crates/gateway` exposes an unavailable integration boundary and dispatches nothing. `apps/openquick` composes them to show status and exercise synthetic transitions without network access.

The library API is for trusted callers in one process. Possession of an opaque ID or an `Identity` value does not prove authentication, tenancy or permission. A future server must authenticate users/services, derive tenant and session context from trusted claims, check approver authority, and prevent untrusted worker/model input from invoking privileged methods directly. There is no network API or access-control service in this slice.

## Request and approval binding

Tenant, session and request identifiers represent separate control-plane concepts. MCP transport session IDs are not assumed to be OpenQuick session IDs. A session scope combines tenant and session IDs. Each request carries a separate explicit identity mode:

- OBO represents work delegated by a user.
- Machine represents M2M application identity independently of a user.

These variants encode identity intent only. There is no token acquisition, signature/issuer/audience verification, consent handling, directory lookup or token exchange. Approval equality distinguishes OBO from machine identity; callers must not use machine authority as a fallback for rejected delegated work. Real credential handling must live in trusted integration components; credentials must never enter model context, worker context, request arguments, approval records, or demo output. Caller-supplied strings are not a secret detector; trusted callers must supply non-secret values.

A submitted tool request is immutable. The scoped request ID cannot be submitted again in the same in-memory control-plane instance, even after completion. Approval must match the complete request, including tenant, session, identity, request ID, tool, target, exact argument bytes and `EffectKind` (`ReadOnly` or `ExternalWrite`). Both effect kinds require approval and use the same terminal outcome behavior; the classification is supplied by a trusted caller, not inferred from a tool. The core compares actual stored values without argument normalization, not a caller-supplied claim that two requests are equivalent. Approval also records the approver principal and requires its scope to match the request; this is attribution, not proof of authenticated authority. Changing the destination, arguments or identity requires a distinct request and approval. Approval is explicit even for a read in this first slice.

`begin` uses a valid approval once and produces an execution permit while marking the request in flight; the approval record remains available for audit attribution. `finish` accepts only a permit from the originating control-plane instance and records a known success/failure or an unknown outcome. A confirmed failure is distinct from a lost response; transport ambiguity must be reported as `Outcome::Unknown`. Cancelled, revoked and unknown requests cannot begin execution. Session cancellation prevents new work and invalidates eligible existing work. Cancelling or revoking after work is already in flight must preserve uncertainty rather than claim that an external side effect was undone.

The request states are explicit:

| Current state | Allowed next states |
| --- | --- |
| `PendingApproval` | `Approved`, `Cancelled`, `Revoked` |
| `Approved` | `Executing`, `Cancelled`, `Revoked` |
| `Executing` | `Succeeded`, `Failed`, `OutcomeUnknown` |
| `Succeeded`, `Failed`, `OutcomeUnknown`, `Cancelled`, `Revoked` | Terminal; no reapproval or retry |

A cancelled session also blocks `submit`, `approve`, `begin` and `finish`. `state` still permits inspection of the recorded terminal outcome.

## Dispatch and uncertainty

An execution permit is a local coordination value, not a bearer credential or a network capability. The current gateway rejects every dispatch attempt. A future adapter must recheck current request/session state at the dispatch boundary and coordinate cancellation with submission; an old permit must not bypass revocation. There is no claimed race-free remote dispatch path yet.

Once external dispatch might have occurred, a timeout, connection loss, process death, or cancellation cannot prove that a write did not happen. Unknown results must stop automatic retries and require independent reconciliation. The first slice keeps an unknown result terminal for that request ID and includes no reconciliation API. It does not provide remote exactly-once execution.

A caller could submit the same semantic action with a different request ID. This slice cannot detect that semantic duplicate; idempotency keys, durable intent/outcome records, provider reconciliation and proxy retry configuration are future integration requirements. AgentGateway's retry settings must be explicitly verified before any write path is enabled.

## Storage and deployment limits

All state is in memory in one process. Restart loses sessions, requests, approvals and outcomes. No durable journal, persistence format, restart recovery, distributed consensus, multi-process coordination or power-loss guarantee is implemented. The core must not be connected to real external writes under an assumption of crash-safe duplicate prevention.

The CLI demo is synthetic and does not run a model or tool. Future work must separately verify IdP integration, credential isolation, transport authentication and policy, cancellation races, safe retry behavior, per-session MicroVM isolation, network egress and BI behavior. This draft provides executable local invariants and an explicit closed integration boundary.
