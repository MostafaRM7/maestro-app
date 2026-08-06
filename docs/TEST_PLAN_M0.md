# Maestro Milestone 0 Test Plan

Status: Execution in progress — local automated candidate gates green;
cross-platform and manual gates pending  
Applies to: [`MILESTONE_0.md`](MILESTONE_0.md)  
Architecture: [`../MAESTRO_ARCHITECTURE.md`](../MAESTRO_ARCHITECTURE.md)

## 1. Purpose

This plan verifies that Maestro's Foundation is a secure and deterministic
platform for the Codex reference adapter. It covers the desktop host, daemon,
authenticated IPC, encrypted storage, process/PTY management, fake-agent event
flow, project/file/Git foundation, and cross-platform UI shell.

Default CI must not invoke, authenticate to, or consume resources from Codex,
Claude Code, or `agy`. A result obtained from a real vendor CLI does not replace
a deterministic Foundation test.

### 1.1 Current automated execution record

On 2026-08-05, the macOS ARM64 development candidate produced this local
evidence:

- strict workspace Clippy passed with warnings denied;
- 213 Rust tests passed with `MAESTRO_FAKE_AGENT` set to the built external
  fixture; two explicit subprocess helpers were intentionally ignored;
- 116 frontend tests passed across 22 files;
- TypeScript, ESLint, production web build, Tauri debug/optimized desktop
  builds, and an unsigned macOS ARM64 `.app` bundle containing `maestrod` and
  `maestro-fake-agent` passed;
- the static provider/IPC boundary scan passed;
- `pnpm audit --audit-level high` reported no known vulnerabilities;
- a five-second non-gating idle-daemon sampler smoke recorded 9.34 MiB maximum
  RSS and 0% sampled CPU.

The short resource smoke verifies the sampler, not the `PERF-*` thresholds.
GitHub target-matrix results, the route-free Linux CI run, launched-webview
flows, full-duration resource measurements, secure-store behavior, menu/tray,
Wayland/X11, and other manual cases remain open until their artifacts are
recorded. Detailed commands, limitations, and evidence handling are in
[`M0_SECURITY_PERFORMANCE_EVIDENCE.md`](M0_SECURITY_PERFORMANCE_EVIDENCE.md).

## 2. Quality gates and severity

- **P0**: security boundary, encryption, data integrity, process isolation,
  protocol correctness, or core lifecycle. Any failure blocks M0 exit.
- **P1**: required user story, platform behavior, accessibility baseline, or
  performance target. Any failure blocks M0 exit unless the product returns to
  explicit design review.
- **P2**: non-blocking polish or diagnostic coverage. It may be deferred only
  with an owner and destination milestone.

A skipped P0/P1 test is a failure unless its platform is explicitly not
applicable. A flaky P0/P1 test is not accepted as green; quarantine requires an
issue and the associated gate remains unsatisfied.

## 3. Test levels

1. **Unit** — state transitions, framing, serialization, redaction, retention,
   path policy, environment policy, and error mapping.
2. **Component** — database worker, key-provider abstraction, process/PTY
   manager, event store, fake CLI, file service, Git parser, and UI components.
3. **Contract** — a common fake-adapter suite proves every adapter operation has
   stable inputs, events, errors, and fallback declarations.
4. **Integration** — packaged or release-built GUI, daemon, database, socket,
   and fake child executables communicate as real processes.
5. **End to end** — user-visible workflows and window/process lifecycle.
6. **Manual platform** — native window, tray/menu, terminal behavior, visual
   theme, Wayland/X11, launch, and OS secure-store checks.
7. **Performance/security** — separately recorded release-build gates with raw
   evidence retained as CI artifacts.

## 4. Required environments

| Environment | Automated responsibility | Manual responsibility |
|---|---|---|
| macOS 13+ ARM64 | Full unit/component/contract/integration suite; primary production build; performance | Keychain, menu bar, multi-window, terminal/TUI, native appearance, launch/resource gates |
| macOS 13+ x86_64 | Full unit/component/contract suite; production build; architecture smoke | Keychain, multi-window, terminal/TUI, launch/resource smoke |
| Ubuntu 22.04 x86_64 Wayland | Full Linux suite; AppImage/deb build viability when introduced | Secret Service and unavailable-service fallback, tray, terminal/TUI, scaling, file/Git, resource gates |
| Ubuntu 22.04 x86_64 X11 | Linux suite may be shared with Wayland where display-independent | Tray, terminal mouse, scaling, multi-window, file/Git, lock-independent M0 behavior |

CI uses clean temporary `HOME`-equivalent fixture roots and temporary runtime
directories. It must never point fake tests at the user's real CLI config,
credential store, projects, or database. Tests seed their own executable search
path and locale/time source.

The suite runs with outbound networking denied. Dependency retrieval happens in
a separate build/setup stage; the test processes themselves need no network.

## 5. Determinism rules

- Fake time, UUID, random-key, and key-provider sources are injectable.
- Fixtures synchronize through protocol messages or observable conditions, not
  arbitrary sleeps. Timeouts use a fake clock where practical.
- Every fake script/scenario is versioned and has a canonical expected event
  transcript.
- Stream-fragmentation tests use a seeded chunk schedule recorded with the
  failure.
- Paths are created under a unique temporary root and canonicalized before use.
- Child processes receive only fixture-controlled environment variables.
- Test output redacts fixture secrets exactly as production output would.
- Concurrency assertions use session/run IDs and monotonic sequences rather
  than arrival wall-clock order across independent sessions.
- Golden terminal recordings include terminal dimensions, locale, color mode,
  input bytes, and expected final VT screen/hash.
- On failure, CI retains sanitized logs, correlation IDs, process exits,
  normalized transcripts, and screenshots where relevant.

## 6. Fake CLI contract

The fake CLI is an external executable spawned through the production process
manager. It must support structured stdio and PTY/TUI modes without importing
production daemon internals.

### Control inputs

The scenario is selected by an explicit argument pointing to immutable fixture
data. Fixtures define:

- deterministic installation name/version/capability response;
- stdout and stderr frames, including exact byte fragmentation;
- expected stdin messages and terminal keystrokes;
- permission and user-input requests;
- child-process creation;
- delay/barrier points controlled by the test harness;
- normal/nonzero/signal exit and ignore-termination behavior;
- structured protocol version and malformed-frame injection;
- output volume and rate;
- expected resume/binding ID;
- PTY dimensions and mouse/cursor behavior.

No scenario may execute a shell command assembled from fixture text. Arguments
are passed as arrays and all created child processes stay inside the test
process group.

### Canonical fixture set

| Fixture | Behavior | Primary assertion |
|---|---|---|
| `structured/happy` | init, streamed content, tool start/end, artifact, usage, result, exit 0 | Exact normalized transcript and terminal console projection |
| `structured/fragmented` | Same transcript split inside frame headers, UTF-8 code points, and payloads | Decoder is byte-safe and emits exactly one copy |
| `structured/multi-frame-read` | Many frames in one write | Decoder drains all frames in order |
| `structured/permission` | Requests command/path approval and waits | Allow/deny/timeout reaches the same process and is audited |
| `structured/user-input` | Requests text and choice input | Response is correlated once; cancellation is explicit |
| `structured/gui-actions` | Handles interrupt, resume, and permission actions | Console annotation precedes/correlates to actual response |
| `structured/nonzero` | Emits valid events, stderr, then exits nonzero | Prior events persist; run fails with exit/correlation details |
| `structured/crash` | Terminates by signal mid-frame | Partial frame is not normalized; other sessions continue |
| `structured/malformed` | Invalid length/type/payload and oversized declaration | Bounded rejection; daemon survives; safe stable error |
| `structured/incompatible` | Unsupported protocol version/capability | Incompatible state and fallback action, never guessed support |
| `structured/stall` | Stops after partial output until interrupted | No busy polling; interrupt/timeout behavior is deterministic |
| `structured/flood` | Sustained small deltas and terminal output beyond buffers | Backpressure policy, ordered persistence, bounded memory |
| `structured/resume` | First run returns binding; second validates it | One logical session, two runs, no concurrent writers |
| `structured/process-tree` | Spawns child and grandchild | Explicit stop cleans the complete owned group |
| `structured/ignore-term` | Ignores graceful termination | Escalation after configured timeout, with audited exit |
| `tui/vt-baseline` | Colors, cursor moves, clear/insert/delete, Unicode | Final VT screen and raw recording match golden data |
| `tui/alternate-screen` | Enters/exits alternate screen and restores main screen | Correct buffer restoration and scrollback behavior |
| `tui/resize-mouse` | Reports size and SGR mouse events | Child observes exact resize and mouse bytes |
| `tui/osc-security` | Emits clipboard, title, hyperlink, and hostile sequences | Clipboard denied, app identity fixed, link untrusted |
| `shell/interactive` | Prompt, stdin, stdout/stderr, job child | Independent real PTY and process-tree ownership |

## 7. Deterministic fake-CLI test matrix

All tests in this section are P0 and automated unless noted.

| ID | Scenario | Assertion |
|---|---|---|
| FCLI-001 | Run `structured/happy` | Stored normalized events, live rich stream, event console, raw live stream, run state, and exit match the canonical transcript |
| FCLI-002 | Fragment every valid frame at seeded byte boundaries | Frame reconstruction is exact; split UTF-8 is valid; sequences contain no gap/duplicate |
| FCLI-003 | Send multiple frames per OS read | All frames emit once and in source order |
| FCLI-004 | Permission allow | One correlated GUI action and one response reach the waiting child; audit event is persisted before broadcast |
| FCLI-005 | Permission deny | Deny reaches the same child once; no persistent rule is created implicitly |
| FCLI-006 | Permission expires/cancels | Child receives documented cancellation/deny; request cannot be reused |
| FCLI-007 | User text/choice request | Input is correlated to the correct session/run/request and never delivered to a peer |
| FCLI-008 | Interrupt a running turn | State follows legal transition through `Interrupting`; process result is captured without losing earlier events |
| FCLI-009 | Resume binding | Second run uses the stored fixture binding; logical event sequence continues monotonically |
| FCLI-010 | Attempt simultaneous writer for same binding | Second writer is rejected deterministically; first remains healthy |
| FCLI-011 | Nonzero exit after valid frames | Session/run failure and stderr are persisted with a stable error and correlation ID |
| FCLI-012 | Signal crash mid-frame | Incomplete payload is discarded/quarantined, run fails, daemon and other runs survive |
| FCLI-013 | Malformed/unknown/oversized frames | Decoder enforces bounds, emits safe incompatibility/protocol errors, and never allocates from an untrusted declared size |
| FCLI-014 | Unsupported protocol version | Handshake fails closed and recommends a fallback; no best-effort parsing occurs |
| FCLI-015 | Stalled process | CPU remains idle; deterministic interrupt/termination unblocks it |
| FCLI-016 | Child ignores graceful termination | Whole group receives graceful signal then force action after configured test timeout |
| FCLI-017 | Flood beyond live-render capacity | Persisted essential events remain ordered; declared nonessential rendering drops/coalesces are measured; memory stays bounded |
| FCLI-018 | Ten mixed structured/TUI/shell sessions | All complete independently with distinct process groups, streams, state, and sequence spaces |
| FCLI-019 | Close all subscribers during output | Process and persistence continue; reconnect replays after last acknowledgement exactly once |
| FCLI-020 | Kill daemon fixture while sessions marked running | Next start marks runs `Interrupted`; UI does not claim exact process continuation |
| FCLI-021 | TUI VT baseline | Final screen, main scrollback, Unicode, color, and cursor state match goldens at each supported terminal size |
| FCLI-022 | Alternate screen | Alternate buffer does not pollute main scrollback and main screen restores on exit |
| FCLI-023 | Resize and mouse | Resize reaches child; SGR mouse press/release/motion/wheel reports match fixture |
| FCLI-024 | Hostile OSC/escape sequence | Clipboard/title/URL policies hold and no webview script/HTML executes |
| FCLI-025 | Seed-like secrets in stdout/stderr/raw/event fields | Event console, logs, errors, and exported test evidence redact by default; live raw view is visibly sensitive |

## 8. Automated subsystem matrix

### Domain, event, IPC, and lifecycle

| ID | Pri | Test |
|---|---|---|
| EVENT-001 | P0 | All legal session transitions succeed; every illegal transition fails without mutating stored state |
| EVENT-002 | P0 | State/event transaction commits before subscribers receive the event |
| EVENT-003 | P0 | Sequence allocation remains monotonic under concurrent producers and daemon reconnect |
| EVENT-004 | P0 | Replay from sequence `n` returns each later event once; expired terminal data is identified explicitly |
| EVENT-005 | P0 | Delta coalescing preserves final user-visible content and tool boundaries |
| IPC-001 | P0 | Correct protocol/auth handshake connects and negotiates one supported version |
| IPC-002 | P0 | Missing, wrong, stale, or replayed authentication material is rejected before privileged requests |
| IPC-003 | P0 | Socket location/permissions restrict access to the owning user; no TCP listener exists |
| IPC-004 | P0 | Truncated, oversized, malformed, and unknown MessagePack messages fail closed with bounded memory |
| IPC-005 | P0 | Slow subscribers cannot block database persistence or unrelated sessions |
| IPC-AUTHZ-001 | P0 | Window/project capability cannot access another project's roots or session commands |
| LIFE-001 | P0 | Single-instance race starts one daemon; loser connects to it or exits safely |
| LIFE-002 | P0 | Closing tab/window detaches only; explicit stop kills only the selected group |
| LIFE-003 | P0 | Daemon stays alive with background processes and exits after configured grace only when no process/client requires it |
| LIFE-004 | P0 | Reconnect restores daemon state to multiple windows without duplicating commands |

### Database, key management, backup, and retention

| ID | Pri | Test |
|---|---|---|
| DB-001 | P0 | Empty store migrates through every version and re-running migrations is safe |
| DB-002 | P0 | Upgrade from every retained schema fixture preserves expected entities and event order |
| DB-003 | P0 | Migration failure rolls back, preserves original encrypted database, and emits an actionable error |
| DB-004 | P0 | No/wrong SQLCipher key cannot query seeded data; correct key can |
| DB-005 | P0 | Database, WAL, terminal segments, and backups do not contain seeded plaintext markers |
| DB-006 | P0 | Concurrent requests serialize through one writer without lock loss or partial state |
| DB-007 | P0 | Backup before migration is encrypted, integrity-checked, and restorable to an isolated location |
| DB-008 | P0 | Seven-daily-snapshot rotation removes only eligible Maestro backups and never vendor data |
| DB-009 | P0 | Corruption handling stops writes and preserves the damaged encrypted file before recovery |
| KEY-001 | P0 | Random 256-bit key is created once, never returned to webview/logs, and loaded from fake secure store after restart |
| KEY-002 | P0 | Unavailable Linux secure store requires passphrase creation/unlock and never selects plaintext fallback |
| KEY-003 | P0 | Wrong passphrase does not alter data; correct passphrase unwraps; passphrase/raw key are absent from disk/logs |
| RET-001 | P1 | Normalized history remains while terminal/raw/debug categories obey independent limits |
| RET-002 | P0 | Raw-frame persistence defaults off and enabling/capping it changes only that category |
| RET-003 | P1 | Retention interrupted mid-run is transactional/idempotent and does not block foreground reads/writes indefinitely |

### Process, PTY, and terminal

| ID | Pri | Test |
|---|---|---|
| PROC-001 | P0 | Executable, arguments, cwd, and environment pass without shell interpolation |
| PROC-002 | P0 | stdout/stderr and exit cause are attributed to the correct run under concurrency |
| PROC-003 | P0 | Graceful stop and force fallback clean child/grandchild process trees without affecting an unrelated group |
| PROC-004 | P0 | Failed executable/cwd/permission reports a stable redacted error with correlation ID |
| PROC-005 | P0 | Controlled environment includes required platform variables, denies fixture secrets, and does not auto-load `.env` |
| PTY-001 | P0 | Independent shell PTYs support input, output, EOF, signal, resize, and exit |
| PTY-002 | P0 | ANSI/Unicode/cursor golden recordings match on supported terminal dimensions |
| PTY-003 | P0 | Alternate screen and mouse reports match canonical fixtures |
| TERM-001 | P0 | Frontend terminal bytes travel only through daemon-owned PTY and preserve ordering |
| TERM-002 | P1 | Scrollback truncates at 10 MB or 50,000 lines per tab and exposes the truncation boundary |
| TERM-003 | P0 | OSC clipboard is blocked by default; title cannot replace Maestro identity; URLs require confirmation |

### Project, files, search, and Git

| ID | Pri | Test |
|---|---|---|
| PRJ-001 | P1 | Create/reopen single- and multi-root projects; recent/favorite/settings persist |
| PRJ-002 | P0 | Duplicate, nested, missing, unreadable, relative, and symlinked roots receive deterministic canonical handling |
| PRJ-003 | P1 | Native folder selection has no application deadline; only post-selection daemon registration is bounded and a timeout is retryable with a correlation ID |
| FILE-001 | P0 | Read/save is restricted to authorized canonical roots and rejects traversal/symlink escape |
| FILE-002 | P1 | Text save is atomic and concurrent disk modification produces conflict UI rather than overwrite |
| FILE-003 | P1 | Binary, oversized, inaccessible, invalid UTF-8, and removed-during-read files fail safely |
| SEARCH-001 | P1 | Literal/regex search streams bounded results, honors Git ignore/hidden policy, supports cancel, and cannot escape roots |
| GIT-READ-001 | P1 | Clean, modified, staged, untracked, ignored, detached-HEAD, unborn, and non-repository fixtures parse correctly |
| GIT-READ-002 | P1 | Rename/binary/large diff handling produces safe display metadata and no raw HTML execution |
| GIT-READ-003 | P1 | Existing linked worktrees are discovered/refreshed; no test invokes worktree create/remove through GUI service |
| GIT-READ-004 | P0 | Git is invoked by argument array with fixture-controlled cwd; malicious filename cannot inject options/commands |

### Frontend and accessibility baseline

| ID | Pri | Test |
|---|---|---|
| UI-001 | P1 | Layout regions render, resize/collapse, persist per window, and recover from missing/corrupt view state |
| UI-002 | P1 | System/light/dark theme changes preserve content and meet defined contrast checks for core controls |
| UI-003 | P1 | Keyboard-only route reaches project navigation, tabs, terminal, panels, dialogs, and window actions without a trap |
| UI-004 | P1 | Configurable shortcuts detect conflicts and never override reserved text/terminal input unexpectedly |
| UI-004A | P1 | The exact shortcut object is validated at the native boundary and round-trips through encrypted daemon-owned settings storage |
| UI-005 | P1 | Supported UI scaling keeps core controls reachable with no clipped modal at tested viewport sizes |
| UI-006 | P1 | Disabled post-M0 actions expose a keyboard/focus-readable explanation and cannot dispatch a command |
| UI-007 | P0 | Markdown/event/diff/error payload corpus cannot inject script, raw HTML, privileged Tauri calls, or unsafe URLs |
| UI-008 | P1 | Virtualized event/file lists preserve focus, selection, and new-event position under updates |

### Security and network

| ID | Pri | Test |
|---|---|---|
| SEC-001 | P0 | Redaction corpus covers common token/key/password/authorization/URL-secret patterns in logs, console, errors, and evidence |
| SEC-002 | P0 | Structured logging prevents raw command environment, database key, passphrase, or fixture credential emission |
| SEC-003 | P0 | Tauri capabilities deny unauthorized filesystem/process/window operations from a compromised webview fixture |
| SEC-004 | P0 | CSP contains no unsafe remote script/source path required by M0 and production assets run locally |
| SEC-005 | P0 | Permission request IDs are scoped, single-use, expire, and cannot approve another session/request |
| SEC-006 | P0 | No persistent rule is created without an explicit remember action; fake vendor/admin deny wins over Maestro allow in evaluator tests |
| NET-001 | P0 | Source/dependency scan finds no OpenAI/Anthropic/Google agent SDK or direct provider client/endpoint implementation |
| NET-002 | P0 | Entire deterministic test suite succeeds with outbound network denied |
| NET-003 | P0 | Runtime socket audit finds authenticated Unix socket only, no Maestro TCP listener, and no provider connection |
| NET-004 | P1 | Telemetry settings default off and core workflows do not enqueue or attempt diagnostic uploads |

## 9. Manual validation

Manual cases are performed from a release build, not a development server.
Record OS/version/architecture, display server, package/commit SHA, secure-store
state, machine hardware, result, screenshots or terminal recording, and issue
links. Use only local fixtures and disposable repositories.

### Common workflow

| ID | Pri | Procedure and expected result |
|---|---|---|
| MAN-PRJ-001 | P1 | Create one single-root and one two-root project; favorite both; open separate windows; restart GUI/daemon. Roots, favorites, and layouts restore correctly. |
| MAN-PRJ-002 | P1 | Open and cancel the packaged native folder picker on the previously freezing macOS folder, leave it open for at least one minute, then select it. The UI remains responsive; any post-selection registration timeout presents a retry path. |
| MAN-TERM-001 | P0 | Run terminal golden fixture; inspect colors, Unicode, cursor editing, full-screen alternate buffer, resize, paste, and exit. Main buffer restores correctly. |
| MAN-TERM-002 | P0 | Exercise mouse-enabled TUI fixture (click, drag/motion, wheel) and verify the child receives expected SGR reports. |
| MAN-LIFE-001 | P0 | Start ten long-running mixed fake/shell sessions; close their tabs and all windows; verify tray/menu state and child continuity; reopen and verify replay. |
| MAN-LIFE-002 | P0 | Explicitly stop one process tree, then all remaining trees. Verify unrelated external process remains alive and no owned child is orphaned. |
| MAN-EVENT-001 | P1 | Run happy, permission, and crash fixtures; compare rich UI, event console, live raw frames, and lifecycle details/correlation IDs. |
| MAN-FILE-001 | P1 | Browse/search 100k fixture, edit/save text, induce concurrent edit, inspect Git status/diffs/worktrees, and verify external-editor action. |
| MAN-UI-001 | P1 | Complete M0 core flows keyboard-only at 80%, 100%, 150%, and 200% scaling; change system/light/dark themes and verify focus/contrast. |
| MAN-NET-001 | P0 | Monitor Maestro/maestrod sockets through first launch and common workflow. Observe no TCP listener or outbound provider traffic. |
| MAN-SEC-001 | P0 | Seed recognizable fake secrets into all fake event channels; verify normal UI/logs/errors are redacted and raw view is clearly marked sensitive. |

### macOS-specific

| ID | Pri | Procedure and expected result |
|---|---|---|
| MAN-MAC-001 | P0 | On macOS 13+ ARM64, first launch creates/uses the expected `com.maestroai.app` Keychain item; relaunch unlocks without exposing key material. |
| MAN-MAC-002 | P1 | Menu-bar session count/actions reflect window closure and background fake/shell sessions; reopening restores windows without stopping children. |
| MAN-MAC-003 | P1 | Multi-window focus, native menus, keyboard shortcuts, system theme, Retina scaling, and external-editor opening behave consistently. |
| MAN-MAC-004 | P1 | Repeat architecture smoke on macOS 13+ x86_64: launch, unlock, project, shell/TUI, background/reopen, clean quit. |

### Ubuntu-specific

| ID | Pri | Procedure and expected result |
|---|---|---|
| MAN-LNX-001 | P0 | With Secret Service available, first launch stores key there and relaunch succeeds without plaintext fallback. |
| MAN-LNX-002 | P0 | With Secret Service unavailable in a clean profile, app remains open at passphrase setup/unlock, creates only encrypted data after input, rejects wrong passphrase, and unlocks with correct passphrase. |
| MAN-LNX-003 | P1 | Under Wayland, validate tray visibility/control, multi-window, theme/scaling, clipboard policy, terminal resize/mouse, and external editor. |
| MAN-LNX-004 | P1 | Repeat the same desktop validation under X11 and compare documented platform-specific limitations. |
| MAN-LNX-005 | P1 | Launch/build smoke on Ubuntu 22.04 x86_64 verifies no dependency on a newer glibc baseline. |

## 10. Performance gates

Performance is measured from optimized release builds with telemetry and raw
frame persistence off. Vendor CLIs do not run. Child fake/shell RSS is reported
separately and excluded from daemon/GUI budgets. Record CPU model, core count,
RAM, storage, OS, architecture, display server, build SHA, and measurement tool.

Run each gate three times unless noted. Report every run and use the worst
steady-state value for memory/CPU. A threshold miss is a failure, not an
observational result.

| ID | Pri | Workload and method | Gate |
|---|---|---|---|
| PERF-START-001 | P1 | Ten launches with GUI and daemon initially stopped, encrypted DB already initialized, no project auto-run. Start timer at process launch; stop when first window has painted, IPC handshake completed, and project-open control accepts input. | p95 <= 3.0 s on each documented reference target |
| PERF-DAEMON-001 | P1 | Open one project, one structured fake session, and one idle shell; wait 60 s, sample daemon RSS for 5 min. | Maximum steady RSS approximately 50 MB or less; any excess requires design review |
| PERF-GUI-001 | P1 | Normal workload: 5,000-event conversation fixture, file tree, one diff, and visible terminal; wait for rendering to settle, sample host/webview aggregate RSS for 5 min. | Maximum steady RSS approximately 250 MB or less |
| PERF-IDLE-001 | P1 | Same normal workload with no streaming/input; after 60 s settle, sample GUI+daemon CPU for 5 min. | Average <= 0.5% of one logical CPU and no unexplained sustained >2% interval lasting 10 s |
| PERF-CONC-001 | P0 | Ten concurrent mixed fake/shell sessions, synchronized start, streamed output, permission waits, resize, and completion. | No event loss/cross-talk/deadlock; remains inside GUI/daemon memory budgets |
| PERF-LEAK-001 | P1 | Repeat create/run/terminate cycle for ten sessions 20 times; force retention/GC where production permits, then settle 60 s. | No monotonic handle/task/process growth; final combined steady RSS no more than 10% or 20 MB (whichever is larger) above settled baseline |
| PERF-FLOOD-001 | P0 | Flood fixture exceeds live-render capacity for 5 min while a second interactive session runs. | Memory is bounded, essential persisted sequence is complete, UI remains interactive, and overflow/coalescing counters explain all nonessential live drops |
| PERF-REPO-001 | P1 | Open generated ~100,000-file Git fixture and expand/search representative paths without semantic index. | Fits GUI/daemon memory budgets; initial tree is lazy (no eager content reads); cancellation responds within 1 s |
| PERF-RET-001 | P1 | Trigger terminal/debug/raw retention at limits while a foreground session streams. | No foreground event gap; no UI stall longer than 1 s attributable to retention |

`Approximately` reflects OS accounting differences, not permission to ignore the
budget. A repeated measurement above the stated target must be profiled and
returned to design review if it cannot be corrected.

## 11. Security gates

M0 cannot exit with an unresolved critical/high security finding. These checks
are mandatory in addition to the automated matrix:

1. **Threat-model review:** trust boundaries include webview/Tauri IPC,
   GUI/daemon socket, local same-user processes, fake/terminal escape streams,
   project symlinks, Git filenames, secure store, encrypted database/backups,
   and export/log sinks.
2. **Encryption evidence:** independent no-key/wrong-key/correct-key checks for
   database and backups; seeded-marker scan covers database, WAL, temporary
   files, scrollback, logs, and crash artifacts.
3. **Key lifecycle:** no raw key/passphrase crosses to the webview, command
   arguments, process environment, log, or disk; secure-store-unavailable state
   has no silent plaintext route.
4. **IPC authorization:** socket is owner-only, handshake authentication fails
   closed, malformed messages are bounded, and project/window capabilities
   prevent confused-deputy access.
5. **Child isolation:** controlled environment drops seeded secrets; `.env` and
   login shell are opt-in; command and Git invocation cannot be shell-injected;
   complete child groups are terminated.
6. **Filesystem authorization:** traversal, symlink escape, race during atomic
   save, hostile filename, and unauthorized-root cases fail safely.
7. **Rendering/terminal:** hostile Markdown/HTML/ANSI/OSC/URL corpus cannot run
   script, call privileged commands, overwrite application identity, or write
   clipboard without approved behavior.
8. **Redaction:** likely tokens, keys, passwords, auth headers, URL credentials,
   and common private-key formats are removed by default from console, logs,
   errors, screenshots metadata, and test/support artifacts. Synthetic values
   only are used.
9. **Network/provider boundary:** production dependency review, source scan,
   outbound-denied suite, and runtime socket capture prove no provider SDK/API
   path and no Maestro TCP listener.
10. **Supply chain:** dependency/license audit has no unresolved forbidden
    license, known critical/high vulnerability, unreviewed install script, or
    remotely hosted production UI asset.

## 12. CI gates and artifacts

Every pull request must run relevant fast checks; protected-branch/nightly jobs
may host longer matrix tests. M0 completion requires a single recorded candidate
SHA with:

- Rust format/lint/test and TypeScript format/lint/type/test reports;
- dependency, license, vulnerability, and provider-SDK scans;
- database migration/encryption/backup/retention report;
- fake CLI contract and 10-session integration transcripts;
- PTY golden comparisons;
- frontend keyboard/security tests;
- macOS ARM64 and x86_64 plus Ubuntu 22.04 x86_64 production-build evidence;
- manual platform checklists;
- performance raw samples and summary;
- security review with disposition of every finding;
- sanitized failure artifacts containing commit SHA, fixture version, seed, and
  correlation IDs.

Secrets, database keys, passphrases, raw user paths, real source code, and real
vendor output must not enter CI artifacts.

## 13. Entry and exit procedure

### Test entry

Testing of an M0 candidate starts when:

- in-scope code is feature-complete and reviewed;
- migrations and fixture versions are frozen for the candidate;
- automated unit/component checks are green;
- a release build exists for the target under test;
- no known issue prevents safe execution of the suite.

### Test exit / Codex handoff

QA signs off the Foundation only when:

- all P0/P1 automated and applicable manual tests pass;
- every `M0-US*` acceptance criterion has linked evidence;
- required target builds and display-server checks are recorded;
- no critical/high security issue, release-blocking defect, unexplained data
  loss, orphan process, plaintext persistence, or provider-network path remains;
- performance gates pass or return to explicit design review;
- flaky release gates are fixed rather than hidden;
- known P2 issues list owner, impact, workaround, and target milestone;
- `MILESTONE_0.md` Codex entry criteria are all demonstrably satisfied.

The Codex milestone receives the candidate SHA, adapter contract version,
schema version, fixture catalog, platform results, performance baseline,
security review, and open-risk register.

## 14. Acceptance traceability

| User story | Primary automated evidence | Manual/performance evidence |
|---|---|---|
| M0-US01 Secure first launch | `DB-*`, `KEY-*`, `SEC-001/002` | `MAN-MAC-001`, `MAN-LNX-001/002`, `MAN-SEC-001` |
| M0-US02 Open project | `PRJ-*`, `FILE-001`, `IPC-AUTHZ-001` | `MAN-PRJ-001` |
| M0-US03 Terminal | `PROC-*`, `PTY-*`, `TERM-*`, `FCLI-021..024` | `MAN-TERM-001/002` |
| M0-US04 Background sessions | `LIFE-*`, `EVENT-*`, `FCLI-018..020` | `MAN-LIFE-001/002`, `PERF-CONC-001` |
| M0-US05 Transparent activity | `FCLI-001..010`, `RET-002` | `MAN-EVENT-001` |
| M0-US06 Failure recovery | `FCLI-011..017`, `IPC-004`, `PROC-003/004` | `MAN-EVENT-001` |
| M0-US07 Files/Git | `FILE-*`, `SEARCH-001`, `GIT-READ-*` | `MAN-FILE-001`, `PERF-REPO-001` |
| M0-US08 Desktop shell | `UI-*`, `LIFE-004` | `MAN-UI-001`, `MAN-MAC-*`, `MAN-LNX-*` |
| M0-US09 Resources | `FCLI-017/018` | `PERF-*` |
| M0-US10 Network boundary | `NET-*`, dependency scan | `MAN-NET-001` |

## 15. Known Foundation test limitations

- Fake CLIs validate Maestro's contract and failure handling, not a vendor's
  current protocol behavior. Each adapter milestone adds versioned fixtures and
  opt-in live conformance.
- CI cannot fully emulate native secure-store prompts, menu-bar/tray behavior,
  compositor differences, or OS process accounting; required manual evidence
  covers these gaps.
- Exact visual native likeness remains a UX review judgment. Functional layout,
  themes, scaling, keyboard access, and resource constraints are measurable M0
  gates.
- Screen-reader-specific behavior and reduced motion are intentionally deferred,
  but M0 must not knowingly prevent their later implementation.
