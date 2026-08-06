# Maestro Milestone 0 — Foundation

Status: Implementation closure candidate — local automated gates green;
cross-platform and manual gates remain open (2026-08-05)  
Product: Maestro (`com.maestroai.app`)  
Source of truth: [`../MAESTRO_ARCHITECTURE.md`](../MAESTRO_ARCHITECTURE.md)  
Target platforms: macOS 13+ (ARM64 and x86_64), Ubuntu 22.04+ (x86_64,
Wayland and X11)

## 1. Outcome

Milestone 0 delivers a secure, testable desktop foundation on which the Codex
reference adapter can be implemented without replacing process, persistence,
terminal, IPC, or window-lifecycle infrastructure.

At completion, a user can open a local project, use ordinary shell terminals,
exercise simulated agent sessions through deterministic fake executables, close
and reopen windows without losing running sessions, review persisted events,
and use basic file and Git inspection. All Maestro-owned persistent data is
encrypted.

Milestone 0 is an internal foundation milestone. It is not a supported agent
release and must not present fake sessions as real vendor integrations.

### 1.1 Implementation closure snapshot

The current candidate implements the Foundation scope with daemon protocol
version 9 and encrypted storage schema version 3. Local macOS ARM64 evidence is
green for strict Rust linting, 213 Rust tests using the real fake-agent
executable, 116 frontend tests, TypeScript checking, ESLint, production web
build, Tauri debug and optimized desktop builds, an unsigned macOS ARM64 `.app`
bundle containing both required sidecars, the provider/IPC boundary scan, and
the JavaScript dependency audit.

Deterministic CI now has a Linux network-namespace job that compiles before
isolation and runs the Rust and frontend suites with no outbound route. The
source/dependency boundary scan rejects direct provider SDKs/endpoints and
daemon TCP/UDP transports while requiring Unix-domain IPC anchors. Resource
sampling tooling records repeatable process RSS/CPU evidence; its short local
daemon smoke is non-gating.

This snapshot does **not** declare Milestone 0 exited. The required GitHub CI
target matrix, launched-webview workflows, full-duration performance runs, and
native macOS/Ubuntu manual gates must still be recorded. See
[`M0_SECURITY_PERFORMANCE_EVIDENCE.md`](M0_SECURITY_PERFORMANCE_EVIDENCE.md)
and [`TEST_PLAN_M0.md`](TEST_PLAN_M0.md) for the evidence and remaining gates.

The bounded, non-production Codex app-server spike is complete for the
installed `codex-cli 0.146.0` on macOS ARM64. It validated the structured path,
froze sanitized fixtures, and introduced internal adapter contract version 1;
see [`CODEX_APP_SERVER_SPIKE_0_146_0.md`](CODEX_APP_SERVER_SPIKE_0_146_0.md).
This does not waive any Foundation exit gate or authorize production Codex
adapter work.

## 2. In scope

### M0-S1 — Repository and build foundation

- MIT-licensed `maestro-app` repository.
- Rust workspace for shared domain types, daemon, native host integration, and
  test support.
- React/TypeScript/Vite frontend hosted by Tauri 2.
- Reproducible development commands and pinned dependency/toolchain policy.
- CI for macOS ARM64, macOS x86_64, and Ubuntu 22.04 x86_64. Jobs may use
  platform-appropriate native runners; Linux artifacts must be built on Ubuntu
  22.04.
- Formatting, linting, type checking, unit tests, dependency/license checks,
  migration tests, and deterministic integration tests.

### M0-S2 — Shared domain and event protocol

- Versioned domain types for projects, workspace roots, logical sessions,
  process runs, turns, normalized events, terminal tabs, capabilities,
  permission requests, and stable errors.
- Legal session-state transitions for `Created`, `Starting`, `Ready`,
  `Running`, `Awaiting permission`, `Awaiting user input`, `Background`,
  `Interrupting`, `Completed`, `Stopped`, `Failed`, `Interrupted`,
  `Recoverable`, and `Incompatible`.
- Per-session monotonic event sequences and reconnect-from-last-acknowledged
  semantics.
- Versioned, length-prefixed MessagePack daemon protocol supporting request,
  response, event, subscription, and binary terminal frames.
- Human-readable console projection and opt-in raw-frame capture for fake
  sessions. GUI actions are recorded as explicit events.
- Bounded channels, streaming-delta coalescing, and defined overflow behavior.

### M0-S3 — Daemon and local IPC

- A single per-user `maestrod` instance with authenticated local Unix-domain
  socket IPC; no listening TCP socket.
- GUI/daemon protocol-version negotiation and actionable incompatibility
  errors.
- On-demand startup, multiple GUI-window connections, reconnect, and clean
  idle shutdown after the configured grace period.
- Daemon ownership of child process groups, PTYs, event persistence, and
  project-scoped native services.
- Crash-safe state recording so unfinished process runs are marked interrupted
  on the next startup.

### M0-S4 — Encrypted application storage

- SQLCipher database opened only by the daemon, with ordered transactional
  migrations and a single-writer design.
- Random 256-bit database key stored by macOS Keychain or Linux Secret Service.
- Required Argon2id passphrase unlock/wrapping fallback when Linux Secret
  Service is unavailable. Maestro may show the unlock shell, but never opens or
  creates application data without encryption.
- Initial schemas needed by the approved architecture, even when later
  milestone features do not yet have a UI.
- Encrypted pre-migration/daily backups, integrity checks, and retention for
  seven rolling daily snapshots.
- Category-aware retention primitives: indefinite normalized history,
  size-limited terminal segments, raw frames disabled by default, and bounded
  debug logs.
- Secret-redaction library and test corpus used before logs or support data are
  persisted or displayed.

### M0-S5 — Process, PTY, and fake-agent harness

- Safe child spawning with explicit argument arrays, working directory,
  controlled environment, process-group ownership, graceful termination, and
  forced process-tree cleanup after timeout.
- Independent PTY sessions for shell tabs and fake exact-TUI sessions.
- Terminal input, output, resize, Unicode, ANSI colors, cursor addressing,
  alternate screen, and mouse reporting.
- Bounded encrypted terminal scrollback with the default limit from the
  architecture (10 MB or 50,000 lines per tab).
- Deterministic fake CLI executable/fixtures that can stream structured events,
  read interactive input, request permissions, simulate delays and malformed
  frames, produce high volume output, exit normally, crash, ignore graceful
  termination, and run as a TUI.
- Ten simultaneously active fake agent/shell sessions without cross-session
  event or input leakage.

### M0-S6 — Desktop and frontend shell

- Tauri application identifier `com.maestroai.app` and product name Maestro.
- One-project-per-window default with multiple native windows connected to the
  same daemon.
- Flexible four-region workspace shell: left navigation, central workspace,
  contextual right panel, and collapsible bottom console, plus global top
  controls.
- Project, Files, Git, Sessions, Events, Raw Protocol, Agent Terminal, and Shell
  navigation surfaces. Later-milestone actions are disabled and explain why.
- xterm-based terminal connected to the daemon's PTY, never to a separate
  frontend-spawned process.
- Light, dark, and system themes; UI scaling; keyboard-reachable resizing,
  navigation, and core actions.
- macOS menu-bar and Linux tray visibility/control for active fake/shell
  sessions. Closing a window detaches views and does not terminate sessions.
- A basic crash/recovery notification surface. Secure notification approval
  actions are not part of M0.

### M0-S7 — Project, file, and Git foundation

- Create/open a project with one or more canonical workspace roots; recent and
  favorite metadata; per-project UI state.
- Lazy, Git-ignore-aware file tree suitable for repositories of approximately
  100,000 files without semantic indexing.
- Text-file view, lightweight edit and atomic save, and repository-wide literal
  or regular-expression search.
- Inline and side-by-side display of existing diffs.
- Read-only Git status/diff/branch inspection and discovery/display of existing
  worktrees through the installed `git` executable.
- Open a file or location in a configured external editor.
- Project-root authorization, canonical-path validation, and symlink-escape
  rejection for GUI file operations.

### M0-S8 — Foundation security controls

- Controlled child environment with allow/deny evaluation and masked preview;
  project `.env` files and full login-shell inheritance disabled by default.
- Narrow per-window Tauri capabilities and a strict content security policy.
- Sanitized Markdown/HTML, untrusted hyperlink confirmation, unsolicited OSC
  clipboard writes disabled, and terminal title isolation.
- Telemetry disabled by default. Foundation execution and tests make no network
  request to an AI provider.
- One-request allow/deny permission plumbing and the core rule-evaluation model
  can be exercised with fake sessions. Persistent-rule management UX and secure
  notification approvals are deferred.

## 3. Explicit exclusions

The following are not acceptance requirements for Milestone 0:

- Any live Codex, Claude Code, or `agy` adapter, probing, authentication,
  configuration, update, session, or provider interaction.
- Shipping an OpenAI, Anthropic, or Google provider API/SDK dependency.
- Claiming feature parity or a supported version range for any vendor CLI.
- Public or externally loaded adapter SDKs/plugins.
- Structured-to-TUI handoff for a real vendor session.
- Persistent permission-rule management UI, notification quick approvals,
  locked-screen approval detection, or dangerous bypass workflows.
- Full conversation UX, model/mode controls, plans, agent tools, MCP/plugin
  management, subagents, remote control, comparisons, or context transfer.
- File rename/move/delete workflows, hunk accept/reject, Git staging/commit,
  branch mutation, stash, fetch/pull/push, PR operations, or worktree creation
  and removal.
- Full import/export, updater channels, release signing/notarization, DMG,
  AppImage, or `.deb` publication. CI must establish target build viability,
  but release packaging is a later gate.
- LSP, IntelliSense, debugging, refactoring, semantic indexing, advanced Git,
  or conflict resolution.
- Ubuntu ARM64, Windows, web, and mobile.
- Screen-reader-specific optimization and reduced-motion support.
- Opt-in live tests that consume vendor resources. Those begin with their
  corresponding adapter milestone.

Deferring an item does not remove it from the approved product. Disabled UI
entries should preserve discoverability when a placeholder is useful, but must
not imply that the feature works.

## 4. Foundation user stories and acceptance criteria

Each criterion is release-blocking unless explicitly marked observational.
Test identifiers refer to `TEST_PLAN_M0.md`.

### M0-US01 — Secure first launch

As a local developer, I want Maestro to protect its data automatically so that
opening the application never creates plaintext history.

Acceptance:

1. With an available OS secure store, first launch generates a database key,
   stores it outside the database, and creates a SQLCipher-encrypted database.
2. Without Linux Secret Service, the application presents passphrase create or
   unlock UI and does not silently continue with an unencrypted database.
3. The database, WAL, terminal segments, and backups disclose no seeded secret
   or known plaintext marker; opening with no/wrong key fails and opening with
   the correct key succeeds.
4. No vendor credential is read, copied, or stored.

Evidence: `DB-*`, `SEC-*`, and `MAN-SEC-*`.

### M0-US02 — Open a local project

As a developer, I want to reopen single- or multi-folder projects so that my
workspace and layout persist.

Acceptance:

1. A project can contain one or more canonical workspace roots.
2. Recent/favorite state and per-window layout survive a daemon and GUI
   restart.
3. A root outside the current project's authorization cannot be accessed by a
   forged frontend request.
4. A symlink that resolves outside all authorized roots is rejected with a
   stable, non-secret-bearing error.

Evidence: `PRJ-*`, `IPC-AUTHZ-*`, and `MAN-PRJ-*`.

### M0-US03 — Use an ordinary terminal

As a developer, I want real shell terminal tabs so that I can run arbitrary
local commands inside my project.

Acceptance:

1. Each tab owns an independent PTY/process group and starts in the selected
   project root with the controlled environment policy.
2. ANSI color, Unicode, cursor control, alternate-screen entry/exit, resize,
   keyboard input, paste, and terminal mouse reports work through the same PTY.
3. Input and output never appear in another tab.
4. Closing a terminal view does not terminate its process; explicit stop
   terminates its process tree and records the exit.
5. Scrollback is encrypted and automatically bounded to the configured limit.

Evidence: `PTY-*`, `TERM-*`, `PROC-*`, and `MAN-TERM-*`.

### M0-US04 — Supervise background sessions

As a developer, I want active work to continue when its window closes so that
window management does not destroy sessions.

Acceptance:

1. Ten fake/shell sessions can run concurrently and retain distinct IDs,
   ordered event sequences, PTYs, and process groups.
2. Closing a tab or all project windows only detaches subscriptions; children
   remain active and visible through menu-bar/tray state.
3. A new window reconnects from its last acknowledged sequence without gaps or
   duplicates.
4. An explicit terminate action stops the selected process tree; an explicit
   terminate-all action stops all owned process trees.
5. After an unclean daemon/OS interruption, formerly active runs are marked
   `Interrupted`; the UI does not claim the interrupted command resumed.

Evidence: `FCLI-018..020`, `LIFE-*`, `EVENT-*`, and `MAN-LIFE-*`.

### M0-US05 — Understand agent activity transparently

As a developer, I want rich, console, raw, and exact-terminal views to share a
logical fake session so that future adapters have one transparent execution
model.

Acceptance:

1. A fake structured session feeds normalized rich components, a readable
   event console, and a live raw inspector from one process run.
2. GUI actions appear in the console as explicit `GUI → CLI` events and map to
   the actual fake-process response.
3. Raw persistence is off by default; enabling it is visible and size-limited.
4. Exact fake TUI mode uses a PTY and is modeled as a process run under the same
   logical session, not as a frontend-only terminal.
5. The system enforces one active writer per logical vendor binding in test
   fixtures.

Evidence: `FCLI-001..010`, `FCLI-021..024`, `EVENT-*`, `RET-002`, and
`MAN-EVENT-*`.

### M0-US06 — Recover from process and protocol failures

As a developer, I want actionable failure states so that a crashed or
incompatible child does not corrupt other work.

Acceptance:

1. Normal exit, nonzero exit, signal exit, startup failure, malformed protocol,
   unsupported protocol version, stalled output, and forced termination produce
   distinct stable errors/states and correlation IDs.
2. A failed session does not terminate or reorder events in other sessions.
3. The final state and available stderr are persisted after output already
   accepted into the event stream.
4. Unknown/malformed frames cannot crash the daemon or render unsanitized
   content in the webview.

Evidence: `FCLI-011..017`, `IPC-004`, `EVENT-*`, `PROC-003/004`, and `UI-007`.

### M0-US07 — Review project files and Git state

As a developer supervising agents, I want lightweight repository inspection so
that I can review changes without turning Maestro into an IDE.

Acceptance:

1. A lazy file tree opens the 100,000-file fixture without eagerly loading file
   contents or building a semantic index.
2. A user can open a supported text file, edit it, and save atomically; a
   concurrent on-disk change produces a conflict warning instead of silent
   overwrite.
3. Repository search honors ignore rules and streams bounded results.
4. Git status, current branch, tracked/untracked changes, inline/side-by-side
   diff, and existing worktrees are displayed from the installed `git` CLI.
5. Binary, oversized, inaccessible, and invalid-encoding files receive safe,
   explicit handling.

Evidence: `FILE-*`, `SEARCH-*`, `GIT-READ-*`, `PERF-REPO-*`, and `MAN-FILE-*`.

### M0-US08 — Use a coherent desktop shell

As a developer, I want a native-like, keyboard-accessible shell so that Maestro
is usable on macOS and Ubuntu before adapter work begins.

Acceptance:

1. Multiple native project windows can connect concurrently and keep view state
   separate while sharing daemon state.
2. Light, dark, and system themes update without losing session state.
3. Core M0 flows are operable by keyboard alone, shortcuts are configurable,
   and UI scaling does not make controls unreachable at supported test sizes.
4. Disabled later-milestone capabilities remain visibly disabled with a reason,
   rather than silently disappearing or executing a placeholder.
5. macOS ARM64, macOS x86_64, Ubuntu Wayland, and Ubuntu X11 validation produce
   recorded evidence.

Evidence: `UI-*`, `MAN-UI-001`, `MAN-MAC-*`, and `MAN-LNX-*`.

### M0-US09 — Stay within the product resource boundary

As a developer, I want Maestro to remain lightweight so that supervising agents
does not compete materially with them.

Acceptance:

1. A usable window is available within 3.0 seconds under the test method in the
   test plan.
2. The daemon remains below approximately 50 MB RSS and the GUI below
   approximately 250 MB RSS during the defined normal workload, excluding fake
   or shell child processes.
3. Idle CPU is near zero according to the explicit test-plan threshold.
4. Ten concurrent fake/shell sessions pass without data loss, deadlock, or
   unbounded memory growth.

Evidence: `PERF-*`.

### M0-US10 — Preserve the provider network boundary

As a privacy-conscious developer, I want proof that Foundation cannot contact
AI providers so that future provider traffic remains CLI-mediated.

Acceptance:

1. The dependency and source audit finds no provider SDK and no direct provider
   URL or client implementation.
2. The full deterministic suite passes with outbound network denied.
3. Runtime network observation during the M0 manual suite shows no listening
   TCP socket and no outbound provider connection by a Maestro process.
4. Telemetry is off by default and no network is required for the core M0
   workflows.

Evidence: `NET-*` and `MAN-NET-*`.

## 5. Milestone 0 definition of done

Milestone 0 exits only when all of the following are true:

- Every in-scope deliverable has an owner, review, and merged implementation.
- All M0 acceptance tests pass on their required platform matrix; no test is
  silently skipped on a required target.
- Deterministic tests use only fake CLIs and local fixtures, run with outbound
  network denied, and are repeatable from a clean checkout.
- The security gates in `TEST_PLAN_M0.md` pass with no unresolved critical or
  high finding. Any accepted medium finding has a documented owner and due
  milestone.
- Database migration, wrong-key, corruption preservation, encrypted backup,
  and Linux passphrase-fallback tests have evidence.
- The 10-session, window-detach/reconnect, PTY compatibility, and process-tree
  cleanup scenarios pass without cross-session leakage.
- Performance budgets are measured on documented reference machines. A miss is
  either fixed or returned for explicit design review; it is not relabeled as a
  pass.
- User-facing Foundation behavior and known limitations are documented.
- CI is green for macOS ARM64, macOS x86_64, and Ubuntu 22.04 x86_64 build/test
  responsibilities.
- There are no unresolved release-blocking defects and no flaky release gate.

## 6. Codex milestone (Milestone 1) entry criteria

A bounded, non-production Codex app-server compatibility spike may run during
Foundation to validate the contract. Production Codex adapter implementation
does not become the active integration milestone until:

1. Milestone 0 meets its definition of done.
2. The internal adapter contract has a reviewed version and fake reference
   implementation covering probe, capability discovery, session start/resume,
   turn input, interruption, permission resolution, feature invocation, TUI
   launch, and health check.
3. Session state, event ordering/replay, protocol errors, child lifecycle, and
   permission primitives are stable enough that Codex does not introduce a
   second execution path.
4. SQLCipher migrations and backup/restore are tested, so Codex fixtures can be
   persisted safely.
5. The rich view, event console, raw inspector, and exact TUI host accept fake
   contract fixtures.
6. The supported initial Codex version range is proposed from official
   documentation and local discovery, with versions pinned in test fixtures;
   unverified versions remain unsupported/TUI-only.
7. Live Codex test policy documents authentication prerequisites, data/resource
   consumption, redaction, opt-in execution, and cleanup. CI remains fake-only
   by default.
8. The Codex app-server compatibility spike has a written result. If it
   invalidates the approved structured path, work returns to design review.

## 7. Codex milestone (Milestone 1) exit criteria

Milestone 1 exits only when:

1. Every user-facing capability in each supported Codex version is present in
   a reviewed capability catalog with support level, maturity, prerequisites,
   fallback, and disabled-state explanation.
2. Installation, version, auth/health detection, app-server handshake, new/
   resume/fork/archive, turns, steering, interruption, plans, tools, diffs,
   artifacts, usage, permissions, user input, models/modes, management commands,
   and exact TUI behavior satisfy the approved Codex scope or have an explicit
   reviewed `CLI-managed`, `PTY-only`, or `Unavailable` classification.
3. Stable structured workflows pass deterministic conformance tests and the
   opt-in live suite on every supported Codex version/platform combination
   claimed by the compatibility matrix.
4. Experimental capabilities are individually version-gated and each has a
   tested fallback.
5. Crash, malformed frame, daemon reconnect, vendor resume, unknown-version,
   and TUI fallback scenarios pass without history corruption or concurrent
   writers.
6. Permission tests prove a Maestro allow cannot override a vendor/admin deny;
   dangerous bypasses require explicit warning/confirmation.
7. Static dependency/source inspection and runtime observation prove Maestro
   uses the `codex` executable and makes no direct OpenAI API call or SDK use.
8. No unresolved critical/high security issue, release-blocking defect, flaky
   conformance gate, or undocumented supported-version limitation remains.

## 8. Change control

- Any proposal to weaken encryption, bypass the CLI-only provider boundary,
  merge structured and exact-TUI processes unsafely, or silently drop a target
  platform requires product/design review.
- A failed required spike returns to architecture review; it does not silently
  reduce a requirement.
- Scope moved out of M0 must retain a named destination milestone and must not
  be represented as complete in the UI or release notes.
