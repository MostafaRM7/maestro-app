# Codex app-server compatibility spike — 0.146.0

Status: viable; production adapter not started  
Observed: 2026-08-05  
Installed CLI: `codex-cli 0.146.0`  
Host: macOS ARM64  
Adapter contract: version 1

## Decision

The approved Codex integration order remains valid:

```text
codex app-server --stdio
    -> codex exec --json
    -> exact PTY/TUI compatibility mode
```

The installed app-server completed its structured handshake and read-only
introspection over newline-delimited JSON. The protocol is suitable for the
reference adapter if Maestro owns correlation, redaction, frame bounds,
process lifecycle, and stable/experimental version gates.

This result does not open Milestone 1. Milestone 0 still requires the packaged,
manual, platform, resource, runtime-network, and CI evidence listed in
`docs/MILESTONE_0.md` and `docs/M0_SECURITY_PERFORMANCE_EVIDENCE.md`.

## Scope and safety

The live probe launched only the installed executable:

```text
codex app-server --stdio
```

It sent `initialize`, `initialized`, `account/read`, `model/list`, and
`thread/list`. No thread or turn was started, so the spike did not intentionally
consume a provider turn. No provider API, SDK, or direct provider URL was used.

Checked-in evidence is synthetic/sanitized. Real account, model, host, path,
thread, Git, and vendor-session values were not copied into the repository.

## Observed wire behavior

### Initialization

The following request shape succeeded with experimental APIs disabled:

```json
{
  "method": "initialize",
  "id": 1,
  "params": {
    "clientInfo": {
      "name": "maestro_spike",
      "title": "Maestro compatibility spike",
      "version": "0.0.0-spike"
    },
    "capabilities": { "experimentalApi": false }
  }
}
```

The response contained `userAgent`, `codexHome`, `platformFamily`, and
`platformOs`. `codexHome` is sensitive local metadata and must be redacted from
normal logs and support artifacts.

An unsolicited `remoteControl/status/changed` notification arrived after the
initialize response and before the client sent `initialized`. Therefore the
adapter must accept and safely queue server notifications as soon as stdout is
read; it must not assume notifications begin only after the acknowledgement.

Handshake guards were explicit:

| Probe | Response |
|---|---|
| request before initialize | error `-32600`, `Not initialized` |
| first initialize | success |
| repeated initialize | error `-32600`, `Already initialized` |
| EOF after requests | clean exit code 0 |

### Correlation and concurrency

`account/read`, `model/list`, and `thread/list` were submitted without waiting
for each other. Responses arrived in request-id order `3`, `2`, `4`, proving the
client must correlate by `id` and never by arrival order.

All three calls can return sensitive or fast-changing data:

- account/authentication state;
- available models, modes, tiers, and defaults;
- local thread identifiers, previews, paths, working directories, Git metadata,
  and resume state.

Raw frames remain available only through Maestro's explicit, bounded sensitive
inspector. Normalized output must be redacted before persistence and fan-out.

### Malformed and large frames

A malformed JSON line was reported on stderr. The same app-server connection
continued to answer a subsequent valid request. Maestro will use a stricter
policy for vendor stdout: malformed structured output poisons that run and
routes recovery toward restart or fallback, because continuing after an
ambiguous vendor frame could corrupt normalized ordering.

The app-server accepted and parsed an unknown-method request containing an
8 MiB string. It returned `-32600` instead of enforcing a small transport
limit. Maestro therefore must not inherit its memory boundary from Codex.

Adapter contract version 1 includes `BoundedJsonLineDecoder` with a default
integration target of 1 MiB per frame. It checks size before extending the
buffer, caps input-batch size and frames per decode call, zeroizes discarded
data, and becomes poisoned after invalid JSON, oversize, fan-out, or incomplete
EOF.

Unknown-method errors listed many method names, including names excluded from
the stable generated schema. Error text is not capability discovery and must
not enable a feature.

## Stable and experimental schema boundary

Schemas were generated from the installed executable twice:

```text
codex app-server generate-json-schema --out <stable-directory>
codex app-server generate-json-schema --experimental --out <experimental-directory>
```

| Schema union | Stable | Experimental | Experimental-only |
|---|---:|---:|---:|
| client requests | 90 | 127 | 37 |
| server requests | 10 | 11 | 1 |
| server notifications | 70 | 70 | 0 |
| client notifications | 1 | 1 | 0 |

The exact experimental-only method list is frozen in
`fixtures/codex/app-server/0.146.0/manifest.json`. Experimental methods are not
part of the initial stable capability catalog. Each future opt-in requires an
individual feature flag, exact version gate, conformance case, security review,
and fallback.

## Initial compatibility policy

The only tested structured version is exactly `0.146.0` on macOS ARM64.

| Installed version | Structured support | Behavior |
|---|---|---|
| `0.146.0` | tested candidate | probe schemas and handshake, then expose reviewed capabilities |
| any other version | untested | warn; probe without enabling inferred features; offer `exec --json` or TUI fallback |
| no app-server/failed handshake | unavailable | try `exec --json`, then exact TUI |

The range must not be widened until that exact executable version has generated
fixtures and passed deterministic plus opt-in live conformance on every claimed
platform.

## Adapter contract result

`crates/maestro-adapter` now defines internal contract version 1:

- identity, executable/version/auth probe, health, and explicit capabilities;
- structured session start/resume with daemon-assigned run identity and atomic
  vendor-binding writer claims;
- turns, steering/follow-up, interruption, permissions, and user input;
- typed model and vendor-authoritative configuration operations;
- session-scoped and global vendor-specific feature invocation for management
  workflows such as authentication, MCP, plugins, updates, and cloud features;
- normalized/redacted events and human-readable GUI annotations;
- separate PTY launch plans with lifetime-bound writer leases for exact TUI
  compatibility;
- redacted Debug output for prompts, arguments, local paths, bindings, feature
  values, and wire frames;
- a deterministic fake reference adapter covering every contract operation;
- a fail-closed 1 MiB JSONL codec that preserves accepted prefix frames before
  terminal protocol errors.

The contract keeps adapters inside the daemon. It does not authorize a desktop
webview to spawn CLIs, create a second process owner, or bypass the existing
event/storage/session supervisors.

## Deterministic fixture catalog

`fixtures/codex/app-server/0.146.0/` contains:

- `manifest.json`: version, transport, boundaries, schema counts, and test
  policy;
- `stable-client.jsonl`: non-consuming handshake/read-only client frames;
- `stable-server.sanitized.jsonl`: synthetic response shapes preserving the
  observed out-of-order correlation behavior.

Tests enforce the 1 MiB decoder boundary, fixture correlation, contract-version
match, non-consuming marker, and absence of common real-local metadata.

## Live-test policy for production work

Default CI remains deterministic and provider-free. Live Codex tests must be
explicitly opted in and must:

1. require an installed, user-authenticated CLI without reading or storing its
   credentials;
2. print the CLI version and platform, but sanitize user paths and IDs;
3. label whether a case can consume turns, credits, or quota before launch;
4. use a disposable repository and synthetic prompts/data;
5. disable raw persistence unless a tester explicitly enables it;
6. redact reports and delete disposable vendor threads when supported;
7. keep auth/login/logout and CLI updates explicit user actions;
8. never run in default public CI.

## Remaining work before production adapter implementation

1. Close and record the outstanding Milestone 0 manual/platform gates.
2. Review adapter contract version 1 and its fake reference implementation.
3. Build the Codex capability catalog from stable 0.146.0 schemas; every method
   needs support level, maturity, auth, security class, fallback, and disabled
   explanation.
4. Implement the daemon-owned app-server state machine with bounded stdout,
   separate bounded stderr, request correlation, timeouts, server requests,
   crash recovery, and single-writer vendor binding.
5. Add deterministic conformance cases for thread start/resume/fork/archive,
   turns/items/deltas, permissions, user input, tools, plans, diffs, usage,
   interruption, malformed output, crash, reconnect, and fallback.
6. Run opt-in live conformance only after deterministic review is green.

Official protocol reference: [Codex App Server](https://learn.chatgpt.com/docs/app-server).
