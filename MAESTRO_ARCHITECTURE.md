# Maestro — Product and System Design

Status: Approved; implementation in progress  
Repository: maestro-app  
Application identifier: com.maestroai.app  
License: MIT  
Last updated: 2026-08-05

## 1. Product definition

Maestro is a local-first desktop control center for orchestrating installed AI
coding-agent CLIs:

- Codex CLI
- Claude Code CLI
- Antigravity CLI (agy)

Maestro provides a unified graphical experience while preserving each vendor's
distinct capabilities. Every AI-provider interaction must pass through the
corresponding executable. Maestro will not import vendor SDKs, call provider
APIs directly, or recreate agent behavior.

### Target user

A single developer working locally across multiple projects and agent sessions.

### Target platforms

Release priority:

1. macOS 13+ ARM64
2. macOS 13+ x86_64
3. Ubuntu 22.04+ x86_64, on Wayland and X11

Ubuntu ARM64, Windows, mobile, and web are not in the initial scope.

### Success criteria

- Detect installed CLIs, versions, authentication state, and capabilities.
- Explicitly represent every capability supported by tested CLI versions.
- Show unsupported features disabled with an explanation.
- Run multiple concurrent sessions and projects.
- Keep active work running after windows close.
- Recover interrupted sessions through vendor-supported resume mechanisms.
- Provide a rich GUI, event console, raw protocol inspector, and exact PTY/TUI
  mode.
- Support cross-agent comparisons without transferring hidden or sensitive
  state.
- Meet the agreed resource and startup budgets.
- Never store vendor credentials or communicate directly with AI providers.

### Non-goals

- Replacing VS Code, JetBrains, or another full IDE.
- LSP, IntelliSense, debugging, refactoring, or deep code indexing.
- Advanced Git merge/rebase/conflict tooling.
- Creating or removing worktrees directly.
- Cloud sync, multi-user collaboration, or Maestro accounts.
- Windows support.
- A public third-party adapter SDK in the first release.
- Screen-reader optimization and reduced-motion support in the first release.

## 2. Recommended technology stack

| Area | Choice | Reason |
|---|---|---|
| Desktop shell | Tauri 2 | Lightweight native host, multi-window support, tray/menu integration, and platform packaging |
| Native core | Rust | Process supervision, PTYs, encryption, filesystem, and Git operations |
| Frontend | React + TypeScript + Vite | Mature ecosystem for a complex, stateful desktop interface |
| UI styling | Custom platform-adaptive design system | Native-like macOS presentation without tying the product to AppKit |
| Lightweight editor | CodeMirror 6 | File review and basic editing without an IDE-scale footprint |
| Terminal | @xterm/xterm | Curses applications, mouse events, Unicode, theming, and optional GPU acceleration |
| PTY layer | Rust portable-pty behind a Maestro abstraction | Shell/TUI processes, input, output, resizing, and process control |
| Async runtime | Tokio | Concurrent child processes, sockets, streaming, cleanup, and backpressure |
| Database | SQLite with SQLCipher | Embedded, transactional, searchable, and encrypted |
| Database access | rusqlite with a dedicated database worker | Low overhead and direct SQLCipher control |
| Secret storage | macOS Keychain / Linux Secret Service | Protects Maestro's database master key |
| IPC | Authenticated Unix-domain socket | Local-only, low overhead, and no listening TCP port |
| Git | Installed git executable | Preserves user configuration, credential helpers, hooks, and exact CLI behavior |
| Repository search | Rust ignore/grep libraries | Fast, Git-ignore-aware search without IDE indexing |
| Packaging | Tauri bundler + GitHub Actions | DMG, AppImage, .deb, and signed updater artifacts |

Tauri channels will carry ordered, high-throughput terminal and agent streams.
Narrow Tauri commands and per-window capabilities will protect privileged Rust
operations from the webview.

References:

- [Tauri IPC and channels](https://v2.tauri.app/develop/calling-rust/)
- [xterm.js](https://github.com/xtermjs/xterm.js/)
- [portable-pty](https://docs.rs/crate/portable-pty/latest)

## 3. High-level architecture

~~~text
┌──────────────── Maestro Desktop Host ────────────────┐
│ Tauri 2                                              │
│                                                      │
│  ┌──────── React/TypeScript windows ──────────────┐   │
│  │ Project UI │ Agent UI │ Diff │ Editor │ xterm │   │
│  └─────────────────────┬──────────────────────────┘   │
│                        │ narrow Tauri commands/channels│
│  Native menus, tray, notifications, window manager   │
└────────────────────────┬─────────────────────────────┘
                         │ authenticated Unix socket
┌────────────────────────▼─────────────────────────────┐
│ maestrod — per-user Rust daemon                      │
│                                                      │
│ Session supervisor       Permission engine           │
│ Process/PTY manager      Environment policy          │
│ Event normalizer         File/Git services           │
│ Retention/export         Adapter registry            │
│                                                      │
│  Codex adapter │ Claude adapter │ agy adapter         │
└────────┬─────────────┬──────────────┬─────────────────┘
         │             │              │
  codex processes  claude processes  agy processes
         │             │              │
         └────── AI-provider traffic through CLIs ─────┘

              ┌──────────────────────────┐
              │ Encrypted SQLite        │
              │ Encrypted backups/blobs │
              └──────────────────────────┘
~~~

This is a modular monolith. The GUI/daemon process split is required so sessions
can survive window closure; it is not a distributed backend.

## 4. Core component boundaries

### Desktop host

Owns:

- Native windows and one-project-per-window behavior
- Menu bar and Linux tray
- OS notifications and quick actions
- Tauri security capabilities
- Update UI
- Rendering and input

It does not:

- Open the database
- Spawn agent CLIs directly
- Store secrets
- Make provider requests

### Core daemon: maestrod

Owns:

- Single-user process supervision
- CLI discovery and version probing
- Logical sessions and process runs
- PTY allocation and resizing
- Adapter execution
- Permission routing
- Event normalization and persistence
- Filesystem, search, Git, exports, and retention
- Database encryption lifecycle

Only one daemon instance runs per OS user. It starts when Maestro launches or
active/background sessions require it, then exits after the last session and GUI
connection disappear following a grace period.

### Adapter modules

Built-in adapters are compiled into the daemon for the first release. They
share the domain model but own vendor-specific:

- Executable discovery
- Capability probing
- Invocation flags
- Protocol parsing
- Permission bridging
- Resume/fork semantics
- Authentication and update workflows
- MCP/plugin/configuration operations
- Vendor-specific UI contributions

### Future adapter host

Rust does not provide a stable dynamic-library ABI. Future third-party adapters
should run out of process and communicate through a versioned JSON-RPC/stdio
protocol.

The first release defines an internal adapter interface but does not publish or
execute external adapters.

## 5. Adapter contract

Conceptually, every adapter implements:

~~~text
identity()
probe(executable) -> Installation + Version + AuthState
discover_capabilities() -> CapabilityCatalog
start_session(spec) -> VendorBinding + ProcessRun
resume_session(binding)
send_turn(input)
steer_or_follow_up(input)
interrupt()
resolve_permission(decision)
invoke_feature(feature_id, arguments)
list_models()
read_configuration()
update_configuration(change)
launch_tui(binding)
health_check()
~~~

### Capability descriptor

Every feature records:

- Stable feature identifier
- Human label and description
- Supported operation and inputs
- Support level:
  - Structured
  - CLI-managed
  - PTY-only
  - Maestro-emulated
  - Unavailable
- Stable or experimental maturity
- Tested version range
- Required authentication or external executable
- Security classification
- Fallback behavior
- Explanation displayed when disabled

Maestro will never infer production support merely because a new flag appears
in --help. Unknown versions receive capability probing, a compatibility
warning, and TUI fallback.

### Shared feature groups

- Session lifecycle
- Turns and messages
- Plans and reviews
- Tools and commands
- File changes and artifacts
- Permissions and input requests
- Models, effort, and modes
- Usage and cost
- MCP servers
- Plugins, skills, hooks, and custom agents
- Background agents/subagents
- Authentication and health
- Update management
- Cloud, browser, IDE, and remote-control features
- Debug and experimental commands
- Exact TUI mode

## 6. Vendor adapter designs

### Codex adapter

Preferred path:

~~~text
codex app-server over stdio
    ↓ unavailable/incompatible
codex exec --json
    ↓ unavailable/incompatible
PTY/TUI
~~~

Codex app-server is intended for rich clients and exposes threads, turns,
streamed items, approvals, history, models, configuration, MCP state, plugins,
and filesystem/process operations. It can generate version-specific TypeScript
or JSON schemas.

Design:

- Launch one app-server process per active structured session initially.
- Perform the required initialization handshake.
- Generate and retain conformance fixtures for every supported Codex version.
- Use thread start/resume/fork/archive operations.
- Normalize turn and item notifications.
- Render server-initiated approval requests as Maestro dialogs.
- Use stable APIs by default.
- Enable experimental APIs only for individually tested capabilities.
- Use codex exec --json for compatible one-shot workflows.
- Launch the original TUI through a PTY for unsupported or terminal-only
  commands.
- Invoke login, logout, update, doctor, MCP, plugin, cloud, review, and debug
  features through the installed CLI.

If profiling shows that one app-server per session is too expensive, compatible
sessions may share an app-server by executable/profile. Isolation is preferred
initially because one failure then affects only one session.

Reference:

- [Codex App Server manual](https://learn.chatgpt.com/docs/app-server)

### Claude Code adapter

Preferred path:

~~~text
claude -p with stream-json input/output
    + Maestro session hooks
    ↓ unsupported capability
Official Claude CLI subcommand
    ↓ unavailable/incompatible
PTY/TUI
~~~

Claude's CLI supports real-time NDJSON, partial messages, session IDs, usage
data, MCP/plugin metadata, subagent relationships, and capability discovery in
newer releases.

No Claude SDK package will be imported. Maestro invokes only the claude
executable.

Permission and input bridge:

- Generate a temporary, session-scoped Claude settings overlay.
- Configure a PreToolUse/PermissionRequest command hook invoking a small
  maestro-hook helper.
- The helper sends the request to maestrod through authenticated local IPC.
- It blocks until the user or a valid Maestro rule responds.
- It returns Claude's documented allow/deny or updated-input response.
- Vendor deny and managed-policy rules continue to take precedence.

Other integrations:

- Resume using vendor session IDs.
- Fork through Claude's supported session flags.
- Represent subagents using parent tool-use identifiers.
- Expose official background-agent and supervisor features through claude
  agents and related commands.
- Expose Remote Control, IDE/browser integration, plugins, MCP,
  authentication, and updates through CLI-managed actions.
- Fall back to the TUI when a version lacks a reliable structured or hook path.

References:

- [Claude programmatic CLI](https://code.claude.com/docs/en/headless)
- [Claude hooks](https://code.claude.com/docs/en/hooks)
- [Claude agent view](https://code.claude.com/docs/en/agent-view)

### Antigravity: agy adapter

Preferred path:

~~~text
agy --print --output-format stream-json
    ↓ terminal-only or interactive capability
PTY/TUI
~~~

The installed agy 1.1.9 was verified locally. Its stream emitted:

- init
- step_update
- result
- Root conversation_id
- Tool inventory
- Permission mode
- Token usage

Design:

- Treat a logical Maestro session separately from an individual agy process.
- Spawn one print-mode process per active turn.
- Persist the returned conversation ID.
- Resume subsequent turns with --conversation.
- Normalize agent response, tool, artifact, usage, checkpoint, and subagent
  steps.
- Use official models, agent, plugin, project, configuration, update, and TUI
  workflows.
- Validate agy hook behavior during its implementation milestone.
- If a structured permission callback is unavailable, use a tested PTY prompt
  bridge:
  - Keep a real PTY attached.
  - Parse terminal state using a VT screen model.
  - Render recognized permission prompts as Maestro dialogs.
  - Send the selected key/input back to that same PTY.
- Any unrecognized terminal state remains available in exact TUI mode.

The agy milestone is not complete until permissions, questions, interruption,
resume, and failure recovery pass live conformance tests.

Reference:

- [Antigravity changelog](https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md)

## 7. Process and session strategy

### Logical session versus process run

A Maestro session is durable and may survive many executable processes.

~~~text
Project
  └── Logical session
        ├── Vendor session/conversation ID
        ├── Turn history
        ├── Process run 1
        ├── Process run 2 after resume
        └── TUI process run
~~~

This handles CLIs such as agy, whose structured process exits after a turn,
without pretending the process remained alive.

### Session states

- Created
- Starting
- Ready
- Running
- Awaiting permission
- Awaiting user input
- Background
- Interrupting
- Completed
- Stopped
- Failed
- Interrupted
- Recoverable
- Incompatible

Every transition is persisted before being published to the GUI.

### Process ownership

- maestrod owns all agent and shell process groups.
- Each regular terminal has its own shell PTY.
- Each TUI session has its own CLI PTY.
- Structured CLI stdout/stderr is captured directly.
- Child processes receive controlled environments and explicit working
  directories.
- Graceful termination is attempted before force termination.
- Process trees are cleaned up as a unit.

### Window and application behavior

- Closing a conversation tab detaches the view; it does not stop the session.
- Closing all windows leaves Maestro in menu-bar/tray mode.
- Continue in background keeps the desktop host and daemon alive.
- Quit UI but keep sessions may terminate the window host while leaving the
  daemon running; tray controls return on relaunch.
- Terminate sessions and quit stops process trees, flushes events, and exits.
- The daemon exits automatically when no active sessions or GUI connections
  remain.

### Terminal views

Every agent session exposes:

1. Rich normalized GUI
2. Human-readable event console
3. Raw protocol inspector
4. Exact TUI compatibility mode

GUI actions create explicit events such as:

~~~text
GUI → CLI: permission.allow(...)
GUI → CLI: session.resume(...)
GUI → CLI: turn.interrupt(...)
~~~

The raw inspector shows actual frames. Persistent raw-frame storage is disabled
by default, while live inspection remains available.

Switching an existing vendor conversation from structured mode to TUI may
require stopping the current process and resuming the vendor session in a new
PTY. Maestro must never open the same vendor session concurrently in two
writers.

### Backpressure

- Persist normalized events before broadcasting them.
- Give each session a monotonic sequence number.
- Reconnect windows using their last acknowledged sequence.
- Coalesce token deltas into small timed chunks.
- Use bounded in-memory channels.
- Spill terminal data to encrypted segmented storage.
- Pause process reads only where the protocol permits; otherwise continue
  persisting while dropping nonessential live rendering updates.

## 8. Internal IPC and event design

### Daemon protocol

Use a versioned, length-prefixed MessagePack protocol over a Unix-domain socket.

Reasons:

- Binary terminal frames without Base64 overhead
- Typed Rust and TypeScript representations
- Lower overhead than local HTTP/WebSocket
- Protocol-version handshake
- Request, response, notification, and subscription multiplexing

Representative requests:

~~~text
system.hello
cli.discover
cli.probe
project.open
session.create
session.resume
session.stop
turn.start
turn.interrupt
permission.resolve
terminal.open
terminal.write
terminal.resize
git.status
git.stage_hunk
export.create
~~~

Representative events:

~~~text
cli.capabilities_changed
session.state_changed
agent.event
permission.requested
user_input.requested
terminal.data
process.exited
retention.completed
notification.created
~~~

### Error convention

~~~json
{
  "code": "CLI_PROTOCOL_INCOMPATIBLE",
  "message": "The installed Claude version emitted an unsupported event.",
  "retryable": false,
  "user_action": "OPEN_TUI",
  "correlation_id": "uuid",
  "details": {
    "adapter": "claude",
    "version": "2.1.197"
  }
}
~~~

Errors must:

- Use stable machine-readable codes
- Distinguish retryable and terminal failures
- Provide a safe recommended action
- Never include secrets in user-visible details
- Carry a correlation ID for support bundles

## 9. Data model

| Entity | Purpose |
|---|---|
| schema_migrations | Ordered database migrations |
| projects | Project identity, display name, and defaults |
| workspace_roots | Single or multiple root folders |
| worktrees | Discovered Git worktrees and status |
| windows | Per-project window layout and state |
| cli_installations | Executable paths, versions, and fingerprints |
| capability_snapshots | Tested and detected feature matrix |
| sessions | Logical Maestro sessions |
| vendor_bindings | Vendor conversation/thread/project IDs |
| process_runs | PID, invocation, channel, exit, and recovery state |
| turns | User request and completion status |
| events | Append-only normalized event stream |
| raw_segments | Optional compressed raw protocol frames |
| terminal_tabs | Shell and agent terminal metadata |
| terminal_segments | Size-limited encrypted scrollback |
| permission_rules | Explicit user-created Maestro policies |
| permission_requests | Requests, decisions, scopes, and audit trail |
| artifacts | Paths, metadata, and diff references |
| file_changes | Diff data without full snapshots |
| comparison_groups | Cross-CLI prompt comparisons |
| comparison_members | Sessions participating in a comparison |
| exports | Export jobs and result metadata |
| settings | Global/project/user-interface preferences |

### Normalized event envelope

~~~text
event_id
session_id
run_id
sequence
timestamp
source: cli | gui | daemon | hook | pty
kind
visibility: user | debug | sensitive
vendor_event_id
payload
raw_segment_reference
~~~

Hidden reasoning will not be requested, normalized for transfer, or exported.
Only user-visible reasoning summaries explicitly surfaced by a CLI may appear
as ordinary content.

### Retention defaults

- Normalized messages and tool events: indefinite
- Terminal scrollback: 10 MB or 50,000 lines per tab
- Persisted raw protocol frames: disabled by default; configurable size cap
- Debug logs: 14 days and 100 MB total
- File information: paths and diffs, no complete snapshots by default
- Encrypted database backups: seven rolling daily snapshots
- Cleanup: automatic, low-priority, and never deletes vendor-owned history

## 10. Encryption and secret handling

Use SQLCipher for the main database and backups. SQLCipher encrypts SQLite
database pages and WAL page data using the database key.

### Key lifecycle

- Generate a random 256-bit database key.
- Store it under com.maestroai.app in macOS Keychain or Linux Secret Service.
- Never expose it to the webview.
- Keep decrypted key material only in daemon memory.
- Wipe temporary buffers where practical.

If Linux Secret Service is unavailable:

- Display a passphrase unlock screen.
- Derive a wrapping key using Argon2id with a random salt.
- Require the passphrase after daemon restarts.
- Never write the passphrase or raw database key to disk.
- The daemon may continue active sessions while unlocked, even after GUI
  windows close.

There is no account recovery. Losing both the secure-store entry/passphrase and
backups makes encrypted Maestro data unrecoverable; onboarding and export
documentation must state this clearly.

Reference:

- [SQLCipher security design](https://www.zetetic.net/sqlcipher/design/)

## 11. Permission architecture

Maestro's permission engine is an overlay, not a replacement for vendor policy.

### Evaluation order

1. Vendor/admin deny policy
2. Maestro explicit deny
3. Vendor explicit ask
4. Maestro matching user-created rule
5. Vendor default behavior
6. GUI prompt

A Maestro allow must never override a vendor deny.

### Supported scopes

- Request
- Session
- Project
- CLI
- Global

Rules support:

- Tool name
- Command pattern
- Canonical path pattern
- Allow or deny
- Expiration
- Creation source
- Inspection, editing, and revocation

Persistent rules are only created through an explicit remember action. Global
and dangerous rules require a second confirmation.

### Notifications

- Low-risk, one-time approvals may use notification actions.
- Persistent, global, destructive, ambiguous, or dangerous actions open a
  confirmation window.
- Approval tokens are single-use and short-lived.
- Approval actions are disabled while the screen is locked.
- If Linux lock state cannot be established reliably, quick approval is
  disabled.
- Dangerous bypass modes are never available from notifications.

## 12. Environment and filesystem security

### Controlled process environment

Default inheritance includes only variables required for normal CLI and desktop
operation, such as:

- User and home-directory identity
- Locale
- Temporary-directory paths
- Display/Wayland and D-Bus information
- SSH agent socket when enabled
- Curated executable search paths
- Vendor-required configuration locations

Not inherited automatically:

- Arbitrary shell-profile output
- Project .env
- Unrelated secret-bearing variables
- Full login environment

Users can:

- Add allow/deny rules
- Preview masked values
- Enable full login-shell inheritance explicitly
- Enable a project .env explicitly

Desktop GUI applications do not normally inherit shell-profile PATH, so Maestro
must implement executable discovery rather than assume terminal PATH behavior.

### Filesystem protections

- Canonicalize project paths.
- Detect symlink escapes.
- Enforce operation roots for GUI file actions.
- Use argument arrays, not shell string interpolation.
- Require confirmation before deleting nonempty directories.
- Use recoverable trash operations where supported.
- Validate atomic vendor-config writes and retain backups.

### Terminal protections

- Disable unsolicited OSC clipboard writes by default.
- Treat terminal hyperlinks as untrusted.
- Confirm external URL opening.
- Prevent terminal title sequences from changing application identity.
- Sanitize HTML/Markdown and disable raw embedded HTML.
- Apply strict Content Security Policy and narrow Tauri capabilities.

## 13. UI architecture

### Window model

- One project per native window by default
- Multiple projects and windows concurrently
- Sessions and tabs detachable or movable between compatible windows
- Global daemon state shared across windows
- Per-window Tauri capabilities scoped to that project's roots

### Layout

~~~text
┌──────────────── Top controls ────────────────────────┐
│ Project │ CLI │ model │ effort │ mode │ session     │
├────────────┬──────────────────────────┬──────────────┤
│ Projects   │ Conversation / plan      │ Tool details │
│ Files      │ Diff / editor            │ Permissions  │
│ Git        │ Terminal / comparison    │ Artifacts    │
│ Sessions   │                          │ Session info │
├────────────┴──────────────────────────┴──────────────┤
│ Events │ Raw protocol │ Agent terminal │ Shell tabs  │
└─────────────────────────────────────────────────────┘
~~~

All panels are resizable, collapsible, and keyboard reachable.

### State ownership

- Daemon state: sessions, turns, events, capabilities, and permissions
- Server-state cache: TanStack Query or equivalent
- View-only state: lightweight local store
- High-frequency streams: dedicated Tauri channels, not global React state
- Large conversations, file trees, and diffs: virtualized rendering

### Shared and vendor-specific UI

- Shared capabilities use normalized components.
- Vendor-specific actions appear under clearly branded sections.
- Unsupported features remain visible but disabled.
- Hover/focus explanations identify:
  - Unsupported CLI
  - Unsupported installed version
  - Missing authentication
  - Experimental feature disabled
  - TUI-only capability

### File and Git tools

Included:

- Lazy file tree
- File content viewer
- CodeMirror lightweight editor
- Repository-wide search
- Create, rename, move, trash/delete
- Inline and side-by-side diffs
- Hunk accept/reject
- Open in configured external editor
- Git status, stage/unstage, commit, branch switch/create, stash, fetch, pull,
  and push
- Existing worktree discovery and monitoring

Excluded:

- LSP and semantic indexing
- Complex conflict resolution
- Worktree creation/removal
- Embedded PR provider APIs

PR operations launch installed tools such as gh through explicit CLI actions or
terminals.

### Comparison and transfer

A comparison group:

- Sends the same prompt independently to selected CLI sessions.
- Displays progress and results side by side.
- Tracks each session's usage separately.
- Never merges vendor state implicitly.

Cross-vendor transfer requires a preview and may include:

- User prompts
- Public plans
- File diffs
- Selected artifacts
- Non-sensitive structured output

It excludes:

- Hidden reasoning
- Raw vendor state
- Authentication
- Secret-bearing environment data
- Unredacted logs

Transfer creates a new prompt through the destination CLI.

## 14. Primary flows and recovery

### First launch

1. Unlock or create the encrypted database.
2. Probe default and configured executable paths.
3. Show installed/missing/version/authentication status.
4. Offer official CLI login/setup actions.
5. Never require all three CLIs.

### Start a session

1. Open or create a project and workspace roots.
2. Choose installed CLI, model, mode, and permissions.
3. Display capability and experimental warnings.
4. Launch the best supported structured channel.
5. Subscribe GUI, event console, and raw inspector.
6. Persist the vendor session binding.

### Permission request

1. Adapter receives a structured request or hook/PTY bridge event.
2. Daemon normalizes and evaluates explicit rules.
3. GUI or secure notification shows exact tool, command, paths, and scope.
4. Decision is audited.
5. Adapter responds through the same CLI session.

### CLI crash

1. Record exit status and final stderr.
2. Mark the run failed and logical session recoverable when possible.
3. Notify the user.
4. Offer restart, vendor resume, TUI fallback, or support bundle.

### Unsupported version

1. Probe version and help/capabilities.
2. Disable unverified structured features.
3. Preserve generic management and exact TUI.
4. Offer an opt-in conformance diagnostic.
5. Never silently guess protocol compatibility.

### System restart

1. Mark previously running process runs interrupted.
2. Restore Maestro history.
3. Probe vendor session availability.
4. Offer or automatically perform safe vendor resume.
5. Never claim continuation of the interrupted command itself.

### Database failure

1. Stop writes.
2. Run integrity checks.
3. Preserve the damaged encrypted database.
4. Offer restoration from an encrypted snapshot.
5. Rebuild Maestro indexes from vendor history where supported.

## 15. Packaging, updates, and CI/CD

### Release targets

| Priority | Target | Package |
|---|---|---|
| 1 | macOS 13+ ARM64 | Signed and notarized DMG |
| 2 | macOS 13+ x86_64 | Signed and notarized DMG |
| 3 | Ubuntu 22.04+ x86_64 | AppImage and .deb |

Use separate macOS architecture artifacts initially. A universal DMG can be
evaluated later, but separate builds simplify native dependencies, diagnostics,
and updater targeting.

References:

- [Tauri distribution guidance](https://v2.tauri.app/distribute/)
- [Tauri AppImage guidance](https://v2.tauri.app/distribute/appimage/)

### GitHub Actions matrix

- macOS ARM64 native runner
- macOS Intel native runner
- Ubuntu 22.04 x86_64 runner
- Separate test, package, sign, notarize, and publish jobs
- Build Linux artifacts on Ubuntu 22.04 to preserve the minimum glibc baseline

Reference:

- [GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)

### Update channels

- stable
- beta

Each channel publishes a signed, per-platform update manifest through GitHub
Releases.

Controls:

- Manual check
- Notify only
- Download automatically
- Install with confirmation
- Never update active CLI sessions without warning

CLI updates:

- Detect availability through official CLI mechanisms.
- Show vendor release/version information when available.
- Invoke only official update commands.
- Require confirmation.
- Re-probe capabilities after completion.
- Do not update while that executable has active processes unless the vendor
  explicitly supports it.

Reference:

- [Tauri updater](https://v2.tauri.app/plugin/updater/)

### CI validation

Required on every pull request:

- Rust formatting, linting, and tests
- TypeScript linting, type checking, and tests
- Dependency and license checks
- Database migration tests
- Fake-CLI adapter contract tests
- PTY golden-recording tests
- Export/import round trips
- Security/redaction tests
- UI keyboard navigation tests
- Production builds for the primary architecture at appropriate checkpoints

Release validation:

- All three native target builds
- Signed updater verification
- macOS notarization
- Installation/upgrade smoke tests
- Performance budgets
- Opt-in authenticated live CLI conformance suite

## 16. Observability, backups, and telemetry

### Local observability

- Structured Rust tracing
- Per-process correlation IDs
- Adapter health view
- Process invocation metadata with secrets removed
- Database and retention health
- Version/capability reports
- Redacted support bundles

### Telemetry

- Disabled by default
- Separate opt-ins for diagnostics and crash reporting
- Preview of submitted data
- No prompts, source code, terminal contents, paths, credentials, or raw
  protocol data by default
- Network destination allowlisted and documented

### Backup/export

- Encrypted automatic local snapshots before migrations and daily rotation
- JSON structured export
- Markdown conversation export
- Sanitized HTML reports
- Complete portable archive
- Selective or complete modes
- Optional passphrase encryption for portable archives
- Prominent warning before plaintext exports

## 17. Performance and scaling

Targets excluding vendor CLI processes:

- Daemon below approximately 50 MB RSS
- GUI below approximately 250 MB RSS during typical use
- Near-zero idle CPU
- Usable window within three seconds
- At least ten simultaneous shell/agent sessions
- Repositories around 100,000 files without IDE indexing

Techniques:

- Lazy project/file loading
- Virtualized lists and diffs
- Incremental Git status refresh
- Filesystem watchers with debounce
- Coalesced streaming deltas
- Bounded terminal buffers
- Compressed cold event segments
- Idle structured-session hibernation where vendor resume is reliable
- One database writer
- No frontend access to the whole event history at once

The modular monolith remains appropriate. Reconsider process pooling or
segmented event databases only if measurements show:

- More than 25 concurrent sessions
- More than 5,000 events per second
- Databases regularly exceeding 10 GB
- Retention jobs blocking foreground work
- One Codex app-server per session creating unacceptable overhead

## 18. Key trade-offs and risks

| Risk | Mitigation |
|---|---|
| CLI protocols change frequently | Tested ranges, capability probes, fixtures, version warnings, and TUI fallback |
| Rich GUI and exact TUI cannot share every transport | Separate structured and exact-TUI modes bound to the same logical session |
| Claude/agy permissions vary by version | Official hooks where supported, tested PTY bridge otherwise |
| Screen parsing is brittle | Version-specific fixtures, VT state parsing, and explicit TUI-only fallback |
| Background daemon crash loses pipes | Mark runs failed, restart daemon, and recover using vendor history |
| SQLCipher complicates builds | Centralized storage crate, migration tests, and native target CI |
| Forgotten Linux fallback passphrase | Clear warning, encrypted exports, and no insecure fallback |
| Linux desktop/lock-state differences | Wayland and X11 tests; disable insecure quick actions when uncertain |
| Vendor configuration writes can corrupt state | Prefer CLI commands, schema validation, atomic replace, and backups |
| Terminal escapes can abuse clipboard/URLs | Disable dangerous sequences and require confirmation |
| Cross-agent transfer loses semantics | Explicit preview and restricted portable context schema |
| Every feature is a moving target | Capability registry and release-specific parity reports |
| App updater is a supply-chain target | Separate signing keys, protected release workflow, provenance, and checksums |

### Required implementation spikes

These are technical validations, not unresolved product choices:

1. Codex app-server schema compatibility across the first supported range.
2. Claude streamed multi-turn lifecycle and blocking hook behavior.
3. agy permission/question behavior in structured mode.
4. Reliable PTY permission detection across terminal sizes.
5. Linux locked-screen detection under Wayland and X11.
6. SQLCipher/Secret Service behavior when the desktop keyring is unavailable.
7. macOS Intel packaging and runtime validation from the ARM-first development
   environment.

If a spike disproves a required behavior, implementation must return to design
review rather than silently weakening the requirement.

## 19. Implementation roadmap

### Milestone 0 — Foundation

Scope:

- GitHub repository and MIT license
- Cargo and frontend workspaces
- Shared domain/event protocol
- Fake CLI fixtures
- maestrod lifecycle and authenticated IPC
- SQLCipher, key management, migrations, and retention
- Process groups, PTYs, and shell terminals
- xterm integration
- Multi-window UI shell, tray/menu bar, and themes
- Project/workspace model
- Basic file browsing, editing, search, diff, and Git inspection
- Initial CI across the three release targets

Acceptance:

- Ten fake concurrent sessions
- Window closure does not stop active sessions
- Encrypted database and backups verified
- Passphrase fallback verified
- Terminal supports ANSI, cursor control, alternate screen, resize, and mouse
- Resource/startup targets measured
- No provider networking exists in Maestro

### Milestone 1 — Codex reference adapter

Scope:

- Installation, version, authentication, and health detection
- App-server protocol and generated schemas
- New/resume/fork/archive sessions
- Turns, steering, interruption, plans, tools, diffs, artifacts, and usage
- GUI permission and input requests
- Models, effort, modes, and images
- MCP, plugins, configuration, review, cloud, remote, debug, and update
  surfaces
- Event console, raw inspector, and exact TUI
- Tested-version capability matrix

Acceptance:

- All user-facing features of supported Codex versions classified and exposed
- Stable structured workflow passes live tests
- Experimental features individually gated
- Crash and resume tests pass
- Unknown-version TUI fallback passes
- No OpenAI API/SDK use outside codex

### Milestone 2 — Claude Code adapter

Scope:

- Streamed CLI integration
- Hook helper and permission/input bridge
- Resume/fork and cost reporting
- Subagents and background-agent management
- MCP, plugins, skills, hooks, and custom agents
- Remote Control, browser/IDE, auth, configuration, update, and debug
- Exact TUI and fallback paths
- Version-specific capability catalog

Acceptance:

- Supported Claude versions pass deterministic and live conformance suites
- Permission rules cannot override vendor denies
- Background sessions survive UI closure
- Unsupported structured operations degrade to CLI/TUI visibly
- No Claude SDK package or direct Anthropic integration

### Milestone 3 — Antigravity adapter

Scope:

- Typed stream-json normalizer
- Conversation ID and per-turn process model
- Resume, project, models, effort, agents, plugins, tools, artifacts, and usage
- Permission and question bridge
- Settings/configuration with safe backup behavior
- Background tasks/subagents
- Exact TUI and fallback
- Version-specific capability catalog

Acceptance:

- agy 1.1.9 baseline fully validated
- Permission, input, interruption, crash, and resume tests pass
- All supported user-facing features are classified
- No direct Google provider integration

### Milestone 4 — Unified product

Scope:

- Cross-CLI comparison groups
- Context-transfer preview and sanitization
- Unified session search/history
- Full import/export
- Permission-rule manager
- CLI/plugin/MCP management center
- Multi-window tab movement
- Notifications and secure quick actions
- Stable/beta updater
- macOS and Ubuntu packaging
- First-release keyboard, scaling, contrast, and theme hardening

Acceptance:

- Same prompt can run across all three CLIs concurrently
- Transfers exclude secrets and hidden/vendor state
- Selective and full exports round-trip
- Signed update validation works on all release targets
- Packaging installs and upgrades cleanly
- Resource targets pass

### Milestone 5 — Release candidate

Scope:

- Security review and threat-model verification
- UI/UX audit
- Code review
- Live compatibility matrix
- Database migration and recovery drills
- Performance profiling
- Signing/notarization
- Documentation and support runbooks
- Dependency/license/SBOM audit

Production-readiness evidence:

- All acceptance suites passing
- No unresolved critical/high security findings
- Verified recovery and rollback
- Signed artifacts and updater
- Published supported-version matrix
- Known limitations documented

### Later iterations

- Public external-adapter protocol and SDK
- Screen-reader-specific optimization
- Reduced-motion support
- Optional universal macOS DMG
- Additional agent CLIs
- Windows, only if later requested

## 20. Approval gate

No implementation, scaffolding, dependency installation, or deployment is
authorized by this document alone.

Implementation begins only after the user explicitly states:

> I approve this design and authorize implementation.
