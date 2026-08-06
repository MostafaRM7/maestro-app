---
name: maintain-maestro-foundation
description: Preserve Maestro's verified daemon, process, IPC, storage, project-capability, terminal, and session-delivery invariants. Use when changing Rust or TypeScript code for CLI adapters, daemon requests, structured sessions, permission or user-input delivery, project registration, window/session reattachment, PTYs, raw protocol inspection, encrypted settings, or lifecycle recovery.
---

# Maintain Maestro Foundation

Apply these guardrails before implementing or reviewing changes that cross the
desktop, daemon, CLI-process, or persistence boundaries.

## Inspect the boundary first

1. Read `MAESTRO_ARCHITECTURE.md`, `docs/MILESTONE_0.md`, and the relevant test
   matrix in `docs/TEST_PLAN_M0.md`.
2. Trace the complete path from webview action through the Tauri host, protocol,
   daemon, child process, storage, and normalized event stream.
3. Treat vendor CLIs as the only allowed agent/provider communication path.
   Never add a provider API, SDK, or direct provider URL.

## Preserve ownership and authorization

- Keep child processes, structured runs, PTYs, scrollback, and live event
  cursors daemon-owned. Closing a tab or window detaches; only an explicit stop
  terminates work.
- Allocate each `RunId` in the daemon before spawning. Pass that exact ID into
  `ProcessSpawner`, adapter construction, persistence, and events; the process
  layer must never generate a competing run identity.
- Issue opaque, window-scoped project and terminal grants in the native host.
  Revalidate persisted project identity, canonical roots, project ownership,
  and integration mode in the daemon.
- Use independent PTYs for shell and exact-TUI compatibility. Structured mode
  and exact TUI are separate projections of a logical session, not competing
  readers of one process.
- Claim one writer atomically for every vendor binding across structured and
  exact-TUI runs. Retain an exact-TUI writer lease until its PTY process has
  conclusively exited and been reaped; never clone the launch spec and drop
  the lease early.
- Keep every daemon-resolved Foundation sibling executable in the native
  bundle. `scripts/stage-sidecar.mjs` and Tauri `externalBin` must stage both
  `maestrod` and `maestro-fake-agent` for the same target triple. Verify the
  final `.app`/Linux package contents; never bundle real vendor CLIs.
- Bump `PROTOCOL_VERSION` for wire-shape or semantic incompatibility and rebuild
  desktop and daemon together. Reject incompatible versions; do not guess.

## Deliver single-use CLI responses transactionally

For permissions and user-input requests, maintain this lifecycle:

```text
active -> in-flight -> resolved
             |-> active             definite pre-delivery failure
             |-> delivery-uncertain ambiguous partial write; retry unsafe
```

- Claim atomically before writing so concurrent responses cannot duplicate.
- Restore the active request only when delivery is definitely absent.
- Publish the normal response audit only after confirmed child delivery.
- Correlate generic feature invocations, results, and console annotations with
  one daemon-assigned operation ID, including concurrent calls to the same
  feature.
- Mark ambiguous or post-delivery failures `retry_safe: false`; the frontend
  must keep the control disabled until an authoritative result/expiry event.
- Re-enable only errors explicitly carrying `details.retry_safe === true`.
- Preserve expiry, stop, capacity, session/run/request scoping, and sensitive
  input redaction in every transition.

## Register projects without UI hangs or duplicates

- Leave the native folder picker without an application deadline.
- Apply the deadline only after selection, while registering with the daemon.
- Correlate timeout errors with the actual wire `RequestId` and make them
  retryable.
- Reuse one `ProjectId` idempotency key for retryable failures of the same
  sorted canonical-root set; use a fresh wire request ID for each retry.
- Atomically reuse an existing persisted project with the exact canonical-root
  set so late completion, restart, or reversed multi-root selection cannot
  create duplicate recent projects or lose favorites/layout.
- Hide the persisted identity behind a fresh window grant after success.

## Bound renderer work and sensitive data

- Stop xterm subscriptions and input handlers while a terminal view is hidden.
  Recreate the renderer and replay bounded daemon history on activation without
  resizing the PTY to an invalid hidden geometry.
- Fetch unredacted raw protocol bytes only while the Raw view is active, clear
  renderer state when hidden, and render bounded pages instead of formatting a
  full capture synchronously.
- Bound adapter JSONL frames to 1 MiB regardless of vendor tolerance. When a
  malformed, oversized, or over-batch tail follows complete frames in one
  read, return and persist the valid bounded prefix before failing the run.
- Virtualize large rich-event, console-event, and file lists with stable keys.
  Keep keyboard scrolling, focused-row pinning, selection, and follow-end
  anchoring correct after measured-height changes.
- Check shortcut ownership before every global shortcut, including F6. Terminal,
  editor, input, select, textarea, and contenteditable targets own their keys.

## Store settings defensively

- Keep Maestro application settings in daemon-owned SQLCipher storage.
- Bound setting scope, reference, key, and JSON size in the protocol/daemon.
- Validate the exact typed object again at the native boundary; reject unknown
  fields, invalid canonical shortcuts, and conflicts.
- Make creation of daemon-owned terminal, raw-protocol, debug, and retention
  directories concurrency-idempotent. Live readers and run finalizers can be
  their first simultaneous creators: accept `AlreadyExists` only after
  verifying the resulting path is a real directory, and continue rejecting
  symlinks and non-directories. Keep a synchronized multi-creator regression
  test with the persistence tests.
- Never copy vendor credentials or make Maestro's database authoritative for
  vendor authentication/configuration.

## Validate proportionally

Run focused regression tests first, then the full Foundation matrix after
cross-boundary changes:

```sh
rtk cargo fmt --all --check
rtk cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
rtk cargo build --locked -p maestro-fake-agent
rtk env MAESTRO_FAKE_AGENT="$PWD/target/debug/maestro-fake-agent" cargo test --locked --workspace --all-targets --all-features
rtk proxy pnpm typecheck
rtk pnpm lint
rtk pnpm test
rtk pnpm build
rtk node scripts/verify-m0-boundaries.mjs
```

Also rebuild the Tauri desktop and daemon pair after protocol/native changes.
Record manual macOS/Ubuntu terminal, picker, multi-window, secure-store,
resource, and runtime-network gates separately; automated component tests do
not replace packaged-webview evidence.
