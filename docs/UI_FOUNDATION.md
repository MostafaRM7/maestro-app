# Maestro UI Foundation

Status: Approved Milestone 0 implementation specification  
Product: Maestro (`com.maestroai.app`)  
Platforms: macOS 13+ ARM64; Ubuntu 22.04+ x86_64 Wayland for Milestone 0;
X11 validation in Milestone 4<br>
Related design: [`../MAESTRO_ARCHITECTURE.md`](../MAESTRO_ARCHITECTURE.md)

## 1. Purpose and scope

This document turns the approved product architecture into an implementable
desktop UI foundation. It is normative for the Milestone 0 shell and provides
the interaction contracts that later agent adapters must reuse.

Milestone 0 must establish:

- The native-window shell, project navigation, panels, tabs, and layout state.
- A consistent way to show logical sessions, process state, permissions,
  capabilities, failures, and recovery.
- A real shell terminal plus the session-level rich view, event console, raw
  inspector, and exact-TUI view contracts.
- Platform-adaptive menus, shortcuts, themes, scaling, focus behavior, and
  common empty/loading/error states.
- UI boundaries that stay responsive with ten sessions and repositories of
  approximately 100,000 files.

This document does not add IDE features, define vendor protocol details, or
change the rule that all AI-provider interaction goes through installed CLI
executables.

## 2. Experience principles

1. **Supervision first.** Current work, required attention, and recovery are
   always easier to reach than file editing or configuration.
2. **One truth, several views.** A session's rich UI, event console, protocol
   inspector, and exact TUI are projections of the same logical session, not
   competing histories.
3. **State is explicit.** Running, waiting, background, failed, recoverable,
   unsupported, and disconnected states use text and iconography as well as
   color.
4. **Vendor differences remain visible.** Shared patterns look consistent;
   vendor-specific features retain their name, identity, and support level.
5. **No silent power.** Potentially destructive actions, persistent permission
   rules, environment expansion, and mode changes disclose their consequences.
6. **Desktop conventions win.** Maestro follows macOS or Linux window, menu,
   shortcut, dialog, and focus conventions where they differ.
7. **Progressive disclosure.** The primary canvas shows the current task. Raw
   frames, process metadata, and diagnostics are available without dominating
   normal work.
8. **Recovery is a first-class flow.** A failure view answers what stopped,
   what was saved, and what the user can safely do next.

## 3. Information architecture

### 3.1 Application-level surfaces

Maestro has five top-level surface types:

| Surface | Ownership | Purpose |
|---|---|---|
| Project window | One project by default | Sessions, files, Git, terminals, and project settings |
| Welcome window | No project | Recent/favorite projects, CLI health, open/create project |
| Settings window | Global | Application, appearance, CLI installations, security, updates, shortcuts |
| Focused utility window | Global or project-scoped | Permission confirmation, recovery, export, support bundle |
| Menu bar/tray | Global | Running-session visibility, attention requests, reopen, quit choices |

Settings uses a single instance. A project can have multiple windows when the
user deliberately moves a tab into a new window, but ordinary opening should
focus its existing primary window rather than create duplicates.

### 3.2 Project-window hierarchy

The persistent hierarchy is:

~~~text
Project window
├── Project switcher and global controls
├── Activity rail
│   ├── Sessions
│   ├── Files
│   ├── Search
│   └── Git
├── Primary sidebar (content of selected activity)
├── Workspace tab strip
│   ├── Agent conversation
│   ├── Plan
│   ├── Diff/review
│   ├── File editor/viewer
│   ├── Session comparison
│   ├── Exact agent TUI
│   └── Shell terminal
├── Context inspector
│   ├── Tools
│   ├── Permissions
│   ├── Artifacts
│   └── Session details
├── Bottom panel
│   ├── Events
│   ├── Raw protocol
│   ├── Agent terminal/TUI
│   └── Shell terminals
└── Status bar
    ├── Daemon/CLI health
    ├── Branch/worktree
    ├── Active-session state
    └── Usage/budget
~~~

The activity rail changes the primary sidebar; it does not replace the active
workspace tab. Opening an item from a sidebar focuses an existing matching tab
or creates one. The context inspector follows the active tab and current
selection. The bottom panel follows the active session unless its tab is
explicitly pinned.

### 3.3 Core navigation objects

| Object | Primary home | Secondary access |
|---|---|---|
| Project/workspace | Native window | Project switcher, Welcome window |
| Logical agent session | Sessions sidebar | Workspace tab, tray/menu bar |
| Process run | Session details | Event console, recovery dialog |
| Permission/input request | Attention queue and modal/sheet | Session inspector, notification |
| File | Files/Search sidebar | Diff, artifact, tool-call links |
| Git change | Git sidebar | Tool result, diff workspace |
| Shell terminal | Workspace or bottom panel tab | New Terminal command |
| CLI installation/capability | Settings | New-session picker, status bar warning |

### 3.4 Sessions sidebar grouping

The default sort is attention first, then recency:

1. Needs approval or user input
2. Failed or recoverable
3. Running
4. Ready/background
5. Completed/stopped

Users can switch to grouping by CLI or worktree. Each row contains vendor icon
and name, session title, compact textual status, relative last activity, and at
most one high-priority badge. Animated progress is shown only while visible;
background rows rely on a static running indicator to avoid idle CPU work.

## 4. Window and platform behavior

### 4.1 Shared rules

- Default model: one project per native window.
- Minimum usable project-window size: 960 × 640 logical pixels.
- Suggested first-open size: 1360 × 860 logical pixels, constrained to the
  available work area.
- Restore size, position, maximized/full-screen state, panel sizes, collapsed
  panels, active activity, and open tabs per window.
- Clamp restored windows into the current display work area after monitor or
  scale changes.
- A tab may move between windows only when the destination window has access to
  the same project roots. Moving a session to another project's window opens a
  new compatible project window instead of broadening capabilities silently.
- Closing a workspace tab detaches its view. It never terminates a session.
- Closing the last project window leaves active/background sessions under the
  daemon and tray/menu-bar control.
- Quit always distinguishes `Keep sessions running` from `Terminate sessions
  and quit` when live processes exist.
- Layout writes are debounced and flushed on window-close initiation.

### 4.2 macOS behavior

- Use the system menu bar with application, File, Edit, View, Session, Terminal,
  Window, and Help menus.
- Use `Command` for application shortcuts and preserve standard commands such
  as Close Window, Minimize, Hide, Full Screen, and Quit.
- Place window controls in the native title bar. Toolbar content must respect
  the traffic-light safe area and remain draggable only in unoccupied regions.
- Project permissions and destructive confirmations use a sheet when they
  belong to one window; application-wide security/unlock flows use a focused
  modal window.
- Closing a window follows macOS semantics and does not imply quitting.
- Use the menu-bar status item only while Maestro has active sessions, an
  attention request, or the user enabled `Always show`.
- Prefer native vibrancy only for restrained chrome regions; content panels use
  opaque semantic surfaces so contrast does not depend on wallpaper.

### 4.3 Ubuntu behavior

- Use native window decoration compatible with both Wayland and X11. Do not
  rely on custom hit-testing for essential move/resize behavior.
- Use `Control` for application shortcuts. Preserve desktop conventions for
  close, full screen, and text editing.
- Modal dialogs are transient to their project window and remain usable when a
  compositor does not honor requested placement.
- The system tray is a convenience, not the only way to recover the app. All
  tray actions also exist in menus/commands because tray availability varies by
  desktop environment.
- If tray support is unavailable, closing the last window with active sessions
  shows a clear background-running choice and the relaunch path is the normal
  application launcher.
- Notification quick approvals are unavailable whenever lock state cannot be
  established reliably.

## 5. Project-window layout contract

### 5.1 Zones and sizing

| Zone | Default | Minimum | Maximum/collapse behavior |
|---|---:|---:|---|
| Activity rail | 44 px | 44 px | Fixed; labels in tooltips |
| Primary sidebar | 260 px | 200 px | 420 px; collapsible |
| Workspace | Flexible | 420 px | Always remains visible |
| Context inspector | 300 px | 240 px | 440 px; collapsible |
| Bottom panel | 240 px high | 120 px | 70% of content height; collapsible |
| Status bar | 24 px high | 24 px | Fixed |

All values are logical pixels at 100% UI scale. Drag handles have a visible
1-pixel divider and at least an 8-pixel pointer hit target. Double-clicking a
divider restores that zone's default. `Reset Window Layout` restores all zone
defaults without closing tabs.

When width is constrained:

1. Collapse the context inspector below 1100 px.
2. Collapse the primary sidebar below 980 px, leaving its activity rail.
3. Never overlay either panel automatically; the user reopens it as a temporary
   drawer at narrow widths.
4. Do not collapse the bottom panel while it contains an input-owning terminal.

### 5.2 Initial shell wireframe

~~~text
macOS traffic lights / Linux title          Maestro — checkout-service
┌──────────────────────────────────────────────────────────────────────────────┐
│ checkout-service ▾  [Codex ▾] [Model ▾] [Mode ▾]   + Session   ⌘⇧P / Ctrl⇧P │
├────┬──────────────────┬──────────────────────────────────┬───────────────────┤
│ ◎  │ SESSIONS         │ tab: Agent 1 ×  | README.md ×   │ SESSION            │
│    │ ● Agent 1        ├──────────────────────────────────┤ Tools          2   │
│ ◉  │   Running · 12s  │                                  │ Permissions    1   │
│    │ ◇ Review tests   │        Active workspace          │ Artifacts      3   │
│ ▣  │   Needs approval │   conversation / diff / file /   │───────────────────│
│    │                  │   comparison / terminal / TUI    │ CLI: Codex         │
│ ⌕  │ + New session    │                                  │ State: Running     │
│    │                  │                                  │ Worktree: main     │
│ ⎇  │                  │                                  │ Usage: …           │
├────┴──────────────────┴──────────────────────────────────┴───────────────────┤
│ EVENTS  1  | RAW | AGENT TERMINAL | SHELL: zsh × | +                       │
│ 14:32:08  GUI → CLI  turn.start(...)                                        │
│ 14:32:09  Codex      tool.started  cargo test                               │
├──────────────────────────────────────────────────────────────────────────────┤
│ ● Daemon connected   main +2 −1   2 running   1 approval   budget: —         │
└──────────────────────────────────────────────────────────────────────────────┘
~~~

At Milestone 0, fake sessions populate the agent surfaces. Regular shell tabs
are real PTY sessions. Adapter-only controls remain present where useful for
layout validation, labeled `Available with Codex milestone` rather than made to
appear operational.

### 5.3 Top controls

- Project switcher opens recent/favorite projects and `Open Project…`.
- CLI, model, effort, and mode selectors describe the pending new session when
  no session is active; with an active session they display that session's
  immutable/current values and only permit supported changes.
- `New Session` is the primary action. It opens a compact configuration popover
  or dialog rather than starting with hidden defaults.
- The right edge contains the command-palette button and a single attention
  control when requests exist. It must not become a row of status icons.
- Unsupported choices remain visible and disabled with a focusable explanation
  in the description region of the selector.

### 5.4 Tabs

- Tabs identify content type with an icon, use a text label, show a dirty dot
  for edited files, and show an attention marker for sessions requiring input.
- Middle-click closes on supported pointers. Keyboard close affects only the
  active tab.
- Closing a dirty editor prompts Save/Discard/Cancel. Closing an active agent
  tab only detaches and shows a transient `Session continues in background`
  confirmation with an Undo action.
- Pinned tabs survive ordinary `Close Other Tabs`; transient preview files are
  replaced by the next single-click file. Double-click or editing pins them.
- A session TUI tab and structured writer for the same vendor binding cannot be
  active simultaneously. Mode switching uses the flow in section 9.5.

### 5.5 Context inspector

The inspector is contextual, not a second navigation tree. For a session it
shows summary sections for pending tools, permissions, artifacts, session/run
metadata, capabilities, and usage. For a file it shows file metadata and Git
state. For a diff it shows selection summary and hunk actions.

Sections with no content are omitted except permissions, which remains visible
when a request is pending. Selecting an event may temporarily reveal the
matching inspector section. The user can pin the inspector to prevent this
follow behavior.

## 6. State and feedback model

### 6.1 Application startup states

| State | Main presentation | Available action |
|---|---|---|
| Starting host | Native-sized shell with static Maestro mark | None; avoid a second splash window |
| Connecting to daemon | Shell skeleton and `Connecting to Maestro service…` | `Show details` after 2 seconds |
| Database locked | Dedicated unlock view; no project data rendered | Enter passphrase, use secure store, recovery help |
| First-run key creation | Short security explanation before creation | Continue, exit |
| Migrating data | Determinate progress when known; windows blocked | View safe details; never cancel mid-migration |
| Ready, no projects | Welcome window | Open folder/workspace, recent projects, CLI setup |
| Daemon unavailable | Recovery view with correlation ID | Retry, restart service, open support details |

Do not show indefinite spinners without explanatory text. At 10 seconds, expose
diagnostics and a retry/restart option appropriate to the current phase.

### 6.2 Loading behavior

- Use skeletons only when the eventual shape is known (session list, file rows,
  conversation items). Use an inline progress indicator for an action of
  unknown shape (Git fetch, CLI probe).
- Preserve already-loaded content during refresh; add a quiet stale/refreshing
  indicator instead of blanking the panel.
- Streamed message content appends in coalesced updates. A separate static
  `Working…` status communicates liveness without continuously animating large
  regions.
- File trees load children on expansion. Search and large histories render in
  pages/virtualized windows and never block the whole workspace.
- Buttons that initiated an operation remain labeled with the operation and
  show busy state; they do not change to generic `Loading`.

### 6.3 Empty states

| Surface | Empty-state message | Primary action |
|---|---|---|
| Welcome / no projects | `Open a project to start supervising agents.` | Open Project… |
| Project / no sessions | `No agent sessions in this project.` | New Session |
| CLI not installed | `<CLI> was not found on this computer.` | Setup guide / locate executable |
| CLI signed out | `<CLI> needs authentication through its official CLI.` | Start CLI login |
| Files | `This workspace contains no visible files.` | Reveal exclusions |
| Search | Before query: guidance; after query: `No matches for “…”` | Clear filters |
| Git clean | `Working tree clean.` | Refresh |
| Events | `Session events will appear here.` | None |
| Raw protocol | `Live raw frames are off because this session has no structured channel.` | Open exact TUI when applicable |
| Terminal | `No terminal tabs.` | New Terminal |
| Permissions | `No requests need your attention.` | View permission rules |

Empty states stay inside their surface and do not use modal dialogs.

### 6.4 Session state presentation

| Domain state | UI label | Visual treatment | Primary action |
|---|---|---|---|
| Created | Created | Neutral outlined dot | Start |
| Starting | Starting… | Progress glyph | Cancel when supported |
| Ready | Ready | Neutral check/dot | Send prompt |
| Running | Running | Accent pulse only in active view | Interrupt |
| Awaiting permission | Approval needed | Warning shield and badge | Review |
| Awaiting user input | Input needed | Accent question badge | Respond |
| Background | Running in background | Static background glyph | Focus |
| Interrupting | Stopping… | Progress glyph | Wait |
| Completed | Completed | Success check | Continue/fork |
| Stopped | Stopped | Neutral stop glyph | Resume/restart |
| Failed | Failed | Error icon | View recovery |
| Interrupted | Interrupted by restart | Warning icon | Check recovery |
| Recoverable | Can be resumed | Recovery icon | Resume |
| Incompatible | TUI only | Compatibility warning | Open TUI |

The row label and accessible name always contain the state; color is
supplementary. `Completed` describes a turn/session outcome, not a guarantee
that every requested code change is correct.

### 6.5 Error taxonomy and placement

- **Field error:** next to the invalid input; focus the first invalid field on
  submit.
- **Action error:** inline in the initiating popover/panel with Retry and safe
  details.
- **Surface error:** replaces only the failed panel while preserving navigation
  and other tabs.
- **Session/process failure:** persistent recovery banner in that session plus
  session-list status and optional OS notification.
- **Daemon/database failure:** window-level blocking recovery surface.
- **Unexpected UI error:** error boundary for the affected workspace tab;
  include Reload View and Copy Redacted Diagnostic ID.

User-visible error content follows this order: plain-language outcome, saved
state, recommended action, optional technical details/correlation ID. Raw stderr
is never presented as the only explanation and is redacted before display.

### 6.6 Recovery surface

A recovery card or dialog must show:

1. Session and CLI identity.
2. Last confirmed logical state and timestamp.
3. Failed process run, exit category, and redacted summary.
4. Whether normalized history and vendor binding were saved.
5. Actions supported by capability data, ordered as:
   - Resume vendor session
   - Restart as a new run
   - Open exact TUI
   - Stop/archive session
   - Create support bundle
6. `What will happen?` text for the selected action.

Never label restart as resume. After an OS restart, say that the prior process
was interrupted and that Maestro will use vendor history; do not imply exact
instruction-pointer continuation.

## 7. Permission and confirmation UX

Permission requests use a sheet/modal that remains above its owning project
window without blocking unrelated project windows. It contains:

- Requesting CLI, session, tool, and normalized risk classification.
- Full command as tokens, working directory, and canonical affected paths.
- Vendor policy outcome when relevant.
- Expandable redacted raw request for diagnostics.
- `Deny` and `Allow once` as immediate actions.
- `Remember…` as a separate disclosure, never a preselected checkbox.

`Remember…` requires explicit rule effect, scope, match pattern, and expiration.
Project/CLI/global scopes summarize the breadth before save. Global rules and
dangerous commands require a second confirmation. Keyboard focus starts on the
least risky safe action; Enter must not approve a dangerous request by default.

Destructive file actions name the exact target. Nonempty directory deletion
uses recoverable trash where available and distinguishes `Move to Trash` from
permanent deletion. Notification quick actions never offer persistent or
dangerous approval.

## 8. Keyboard and focus architecture

`Mod` means Command on macOS and Control on Ubuntu. All shortcuts are
configurable unless they are reserved by the operating system.

### 8.1 Required default shortcuts

| Action | Shortcut |
|---|---|
| Command palette | `Mod+Shift+P` |
| Open project | `Mod+O` |
| New agent session | `Mod+Shift+N` |
| New shell terminal | `` Mod+` `` |
| Quick-open file | `Mod+P` |
| Project search | `Mod+Shift+F` |
| Toggle primary sidebar | `Mod+B` |
| Toggle context inspector | `Mod+Shift+B` |
| Toggle bottom panel | `Mod+J` |
| Focus next major zone | `F6` |
| Focus previous major zone | `Shift+F6` |
| Next workspace tab | `Ctrl+Tab` |
| Previous workspace tab | `Ctrl+Shift+Tab` |
| Close active tab | `Mod+W` |
| Save active file | `Mod+S` |
| Send prompt | `Mod+Enter` |
| Insert newline in prompt | `Shift+Enter` |
| Interrupt active agent turn | `Mod+.` |
| Focus pending attention | `Mod+Shift+A` |
| Reset terminal focus to shell UI | `Mod+Shift+Escape` |

### 8.2 Focus rules

- On window open, focus the active workspace's meaningful control; on the
  Welcome window, focus the project list or Open Project button.
- `F6` cycles top controls → activity/sidebar → workspace → inspector → bottom
  panel → status bar, skipping collapsed zones.
- Within lists/trees, arrow keys move, Right expands, Left collapses/returns to
  parent, Enter opens, and Space toggles selection where multi-select exists.
- `Escape` closes the topmost non-destructive popover, exits transient preview,
  or returns focus from a modal. It never interrupts a process unless the exact
  TUI owns focus and receives the key.
- Opening a dialog traps focus; closing it returns focus to the invoking control
  or the nearest surviving parent.
- Focus rings are always visible for keyboard navigation and are never conveyed
  only by a subtle background change.
- Disabled controls are omitted from Tab order only when an adjacent, focusable
  description communicates why. In menus/selectors, disabled capabilities can
  receive roving focus so their explanation is reachable.

### 8.3 Terminal key ownership

When xterm owns focus, terminal input receives ordinary keys, CLI shortcuts,
mouse tracking, and escape sequences unchanged. Maestro intercepts only the
documented application-reserved chords, native menu commands, and the terminal
focus-reset chord. A visible but unobtrusive hint appears the first time an
alternate-screen TUI captures mouse input.

Copy behavior follows platform conventions. Paste warns only when terminal
bracketed-paste protection is unavailable and the text is multiline or appears
to contain a dangerous command. Unsolicited OSC clipboard writes remain
disabled.

## 9. Session consoles and terminal relationships

### 9.1 Four agent-session views

| View | Source | Editable/input behavior | Persistence default |
|---|---|---|---|
| Rich GUI | Normalized events and adapter actions | Prompts, approvals, feature controls | Conversation/events indefinite |
| Event console | Human-readable normalized audit events | Filter/copy; GUI actions appear as annotations | Normalized events indefinite |
| Raw protocol inspector | Exact structured frames, redacted for display/export | Filter/copy; never writes to process | Live only; persisted frames off by default |
| Exact TUI | Dedicated PTY attached to actual CLI | Full terminal input and mouse | Size-limited terminal scrollback |

Regular shell terminals are separate PTY sessions. They are project tools, not
agent-session mirrors, even when the user manually launches an agent CLI in
one.

### 9.2 Event console contract

The event console is a readable ledger, not a fake shell transcript. Each row
contains timestamp, direction/source, category, summary, and optional duration.
Rows expand to normalized details and link to the related message, tool, file,
run, permission, or raw frame.

GUI actions are explicit, for example:

~~~text
14:32:08.144  GUI → Codex   turn.start(prompt_ref=turn:42)
14:32:09.021  Codex → GUI   tool.started(command="cargo test")
14:32:11.818  GUI → Codex   permission.allow(request=prm:8, scope=request)
14:32:15.403  Daemon        process.exited(code=0, run=run:19)
~~~

Sensitive values are redacted before reaching presentation. Filters cover
messages, tools, permissions, lifecycle, errors, and GUI actions. `Follow` is on
by default and pauses when the user scrolls away; a `Resume · N new events`
button returns to live output.

### 9.3 Raw protocol inspector contract

Raw frames show receive/send time, stream/direction, protocol kind, byte size,
sequence correlation, and formatted payload. Binary data defaults to metadata
plus safe hex preview. Redaction status is always visible. Enabling persistence
requires a settings confirmation that states the size limit and sensitivity
risk; debug mode that weakens redaction has a persistent warning.

The inspector must not parse frames on the React render thread. Large frames
are truncated in the list and loaded on demand in a detail pane.

### 9.4 Bottom panel and workspace terminals

- A terminal can live in the bottom panel for observation or be promoted to a
  full workspace tab without creating a second PTY.
- The tab header distinguishes `Events`, `Raw`, `Agent TUI`, and `Shell` and
  shows which session/process each is pinned to.
- Switching active workspace sessions retargets unpinned Events/Raw views.
- A terminal with active keyboard focus is never silently retargeted.
- Closing a shell terminal tab asks before terminating a process with foreground
  activity; otherwise it closes the PTY normally.
- Hiding the bottom panel does not resize a PTY to zero. It retains its last
  valid geometry and receives a resize when shown again.

### 9.5 Structured-to-TUI mode switch

Because one vendor binding cannot have two writers, `Open in exact TUI` uses a
disclosed transition:

1. Explain whether the current structured run must stop and whether the vendor
   session can resume in TUI.
2. If a turn is running, offer Cancel or `Interrupt and switch`; never silently
   kill it.
3. Persist the final structured event/sequence and release the process writer.
4. Start the actual CLI in a new PTY using supported resume behavior.
5. Retain rich history as read-only and label the current transport `Exact TUI`.

Returning to structured mode repeats the writer handoff. If the adapter cannot
resume safely, offer a new linked session rather than presenting continuity.

## 10. Visual system

### 10.1 Semantic color tokens

Components consume semantic roles, never hard-coded vendor colors:

~~~text
--color-canvas
--color-surface-1
--color-surface-2
--color-surface-raised
--color-border
--color-border-strong
--color-text
--color-text-muted
--color-text-disabled
--color-accent
--color-accent-hover
--color-focus-ring
--color-success
--color-warning
--color-danger
--color-info
--color-selection
--color-terminal-background
--color-terminal-foreground
~~~

Light, dark, and system themes are required. `System` updates without restart.
Status colors meet at least WCAG AA contrast for text-sized marks against their
surface and are always paired with a glyph/label. Vendor identity appears in
icons, names, and restrained accents; it does not recolor whole workspaces.

### 10.2 Geometry and spacing tokens

~~~text
--space-1: 4px       --radius-small: 4px
--space-2: 8px       --radius-medium: 7px
--space-3: 12px      --radius-large: 10px
--space-4: 16px      --control-small: 24px
--space-5: 24px      --control-medium: 30px
--space-6: 32px      --control-large: 36px
--space-8: 48px      --divider: 1px
~~~

macOS uses slightly softer radii and restrained translucency in chrome. Linux
uses the same layout metrics with opaque surfaces and platform system fonts.
Do not emulate Aqua controls or a particular Linux desktop theme pixel for
pixel; adapt behavior and typography while keeping Maestro recognizable.

### 10.3 Typography

- UI font: platform system sans-serif.
- Default UI size: 13 px macOS, 14 px Ubuntu.
- Compact metadata: one step smaller, never below 11 logical pixels.
- Monospace: user-configurable platform monospace for terminals, protocol,
  command, paths, diffs, and code.
- Conversation prose uses a comfortable 1.5 line height and a bounded readable
  line length; tool/event surfaces remain denser.
- Use tabular numerals for timestamps, usage, line numbers, and durations.

### 10.4 Scaling and density

Application UI scale options are 80%, 90%, 100%, 110%, 125%, 150%, 175%, and
200%, in addition to OS display scaling. The root UI metrics, xterm font/cell
measurement, CodeMirror text, pointer hit targets, panel defaults, and icons all
recompute from one scale setting. Never scale by applying a blurry CSS transform.

At 80–90%, interactive pointer targets stay at least 24 logical pixels. At
150%+, constrained-width collapse rules use scaled content dimensions rather
than raw physical pixels. Density (comfortable/compact) may reduce whitespace
but is separate from accessibility scaling.

### 10.5 Motion

Keep transitions under 160 ms for panel/tab state and avoid perpetual chrome
animation. Streaming output itself conveys activity; it does not need shimmer.
Although a full reduced-motion setting is deferred, honor the OS preference for
nonessential transitions from the first implementation because the design
system can do so with negligible complexity.

## 11. Component and data boundaries

The initial frontend shell should be composed around these stable boundaries:

~~~text
AppBootstrap
├── UnlockOrRecoveryGate
├── WelcomeWindow
└── ProjectWindow
    ├── ProjectToolbar
    ├── ActivityRail
    ├── PrimarySidebar
    │   ├── SessionList
    │   ├── FileTree
    │   ├── SearchResults
    │   └── GitChanges
    ├── WorkspaceTabs
    │   ├── ConversationView
    │   ├── DiffView
    │   ├── FileView
    │   ├── ComparisonView
    │   └── TerminalView
    ├── ContextInspector
    ├── BottomPanel
    │   ├── EventConsole
    │   ├── RawInspector
    │   └── TerminalView
    └── StatusBar
~~~

Daemon-owned data (projects, sessions, capabilities, events, permissions) is
read through a server-state layer. Window geometry, active tabs, selection,
panel sizes, and follow/pin state are view state. Terminal byte streams and
high-frequency event deltas bypass global React state; components subscribe to
bounded channels and request history by sequence/page.

All long lists use stable IDs and virtualization where needed. Components must
not assume a CLI feature exists; a capability descriptor determines label,
enabled state, support level, explanation, maturity, and fallback action.

## 12. Content and trust rules

- Commands, paths, scopes, versions, and process states use exact text. Do not
  soften dangerous behavior with vague labels such as `Proceed`.
- Buttons use verb-object labels: `Resume Session`, `Open Exact TUI`, `Move to
  Trash`, `Allow Once`.
- Experimental, TUI-only, emulated, and unavailable capabilities have visible
  badges and plain-language explanations.
- Markdown from CLIs is sanitized; raw embedded HTML is disabled. External URLs
  display their destination and require confirmation according to trust policy.
- Secret redaction is represented as `[REDACTED]`; do not make removed data look
  like an empty value.
- The UI never asks for or stores vendor tokens. Authentication surfaces say
  that the official CLI owns the flow and credentials.

## 13. Milestone 0 UI acceptance checklist

- Welcome, unlock, connecting, ready, empty-project, and daemon-failure states
  render without a working adapter.
- Project windows restore safely after resolution/display changes.
- Primary sidebar, context inspector, and bottom panel resize, collapse, reset,
  and persist per window.
- All major zones and shell controls are reachable by keyboard; focus returns
  correctly from dialogs and tab closure.
- Light, dark, and system themes update every semantic token, CodeMirror, and
  xterm without restart.
- Every supported UI scale reflows without clipping the primary action or
  permission dialog.
- Fake sessions exercise every session state, attention order, event-console
  link, recovery state, and capability-disabled explanation.
- A real shell terminal supports ANSI, cursor control, alternate screen, resize,
  Unicode, keyboard input, paste, and mouse reporting.
- Promoting a terminal between bottom panel and workspace preserves the same
  PTY and scroll position.
- Event and terminal rendering remains interactive under ten concurrent fake
  streams; inactive views do not continuously animate.
- Window/tab closure never terminates an agent session implicitly.
- macOS and Ubuntu native menus expose all essential tray/menu-bar actions.
- Error surfaces expose redacted correlation IDs and never leak raw secrets.

## 14. Unresolved implementation risks

These are validation risks, not product decisions to fill in silently:

1. **Tauri native title-bar consistency.** Traffic-light safe-area behavior and
   draggable-region accessibility must be tested on macOS ARM64;
   native Linux decoration must be verified on GNOME Wayland in Milestone 0 and
   on X11 under `M4-LNX-X11-001`.
2. **Cross-window tab movement.** Tauri webviews cannot be reparented as ordinary
   DOM nodes. The implementation should serialize view state and reconnect to
   the daemon stream by last acknowledged sequence, then measure visible handoff
   latency.
3. **Terminal ownership and reparenting.** Moving an xterm presentation must not
   recreate or duplicate its PTY. Resize/focus races need integration tests,
   especially for alternate-screen mouse applications.
4. **Keyboard collisions.** Codex/Claude/agy TUIs and common shells may use
   application-reserved chords. Test the reserved set against supported TUIs
   and make all non-OS bindings configurable.
5. **Linux tray and locked-screen variance.** Tray availability and lock-state
   detection differ across desktop setups. Essential recovery must remain in
   ordinary windows and dangerous notification actions must fail closed.
6. **Large virtualized mixed-height streams.** Markdown, tool cards, and diffs
   produce unstable heights during streaming. Scroll anchoring and focus must be
   tested with long sessions rather than relying only on synthetic fixed rows.
7. **System theme and scale changes while a TUI runs.** xterm cell geometry must
   be recalculated and the PTY resized once, without corrupting terminal state
   or triggering resize loops.
8. **Webview resource ceiling.** Multiple native windows each carry a webview.
   The 250 MB normal-use GUI target needs measurements with at least two project
   windows before committing to always-live hidden views.
