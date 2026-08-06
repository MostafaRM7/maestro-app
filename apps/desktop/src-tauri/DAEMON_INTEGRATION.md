# Desktop-to-daemon terminal contract

The current Foundation contract is daemon protocol version 9 with encrypted
storage schema version 3. Both versions are negotiated or reported through the
authenticated system snapshot; incompatible protocol versions fail closed.

The desktop host connects to the per-user `maestrod` Unix socket for every
command. Each connection performs the authenticated protocol-version
handshake. Terminal ownership remains in `maestrod`, so closing a webview or
dropping a client connection does not stop the PTY.

Tauri uses its default camel-case argument mapping:

- `system_snapshot()`
- `terminal_open({ cwd, columns, rows })`
- `terminal_write({ terminalId, data })`
- `terminal_resize({ terminalId, columns, rows })`
- `terminal_read({ terminalId, afterSequence, maximumBytes })`
- `terminal_state({ terminalId })`
- `terminal_close({ terminalId })`

`terminal_read.nextSequence` is the cursor for the following `afterSequence`.
When `overflowed` is true, the cursor has advanced past discarded live-buffer
chunks and `droppedThroughSequence` identifies the truncation boundary.

The desktop host first attempts an authenticated connection to the per-user
daemon. When the socket is absent, it discovers and starts only a trusted
Maestro-owned `maestrod` executable, then waits up to three seconds for the
authenticated protocol handshake. Authentication or protocol rejection never
causes a replacement daemon to be launched.

Development discovery checks the workspace `target/debug` output. Production
packages embed `maestrod` and the deterministic `maestro-fake-agent` fixture as
Tauri external binaries. The Tauri pre-build step runs
`scripts/stage-sidecar.mjs`, which builds both executables for
`TAURI_ENV_TARGET_TRIPLE` and copies them to the target-triple-suffixed paths
required by Tauri. Keeping the fake executable beside the daemon preserves the
daemon's fixed sibling discovery without accepting a webview-supplied path. The
launcher accepts the packaged daemon sibling/resource paths, rejects symlinks
and non-executable files, closes inherited stdio, and reaps the sidecar
asynchronously while the daemon owns its own process group.

Closing a webview still only detaches it. The per-user daemon and its child
sessions remain independent of that window lifecycle.

Global keyboard shortcuts use the same authenticated daemon connection and the
existing encrypted `settings` table. The daemon enforces bounded setting
identity/value fields and syntactically valid JSON; the native host additionally
requires the exact conflict-free shortcut object before data crosses the
webview boundary. No vendor configuration or credential store is involved.

## Shared-session projections

Structured fake-agent runs and exact fake TUI runs are both daemon-owned
process runs under persisted logical sessions. Structured mode exposes three
projections from the same run:

- normalized redacted events for the rich GUI;
- a human-readable event console including explicit `GUI → CLI` annotations;
- an opt-in sensitive raw-protocol inspector backed by exact pre-decoding
  stdout bytes.

Raw capture must be enabled before launch. It is disabled by default, capped at
1 MiB per run, encrypted in SQLCipher, and read through bounded 256 KiB IPC
pages. Exact TUI compatibility uses a separate PTY execution mode because an
original alternate-screen TUI and structured stdio/app-server protocol may be
mutually exclusive. The daemon consumes the adapter's complete TUI launch plan,
passes the daemon-assigned run identity into the PTY spawner, and retains any
opaque vendor-binding writer lease until the child has exited and the process
supervisor has completed its reap path.

## Project and window authorization

The webview never receives a raw filesystem authority. Opening or restoring a
single- or multi-root project produces an opaque, window-scoped project grant.
File, search, Git, shell, session, TUI, and external-editor commands are
validated against that grant in the native host and again against the
daemon-owned canonical project registration. Moving a session or opening a new
native window establishes an independent grant rather than sharing frontend
authority.

Terminal and session attachment grants are also opaque and window-scoped.
Reattaching replaces only that window's previous attachment; it does not stop
or transfer ownership of the daemon process.

## Persistence and terminal safety

The daemon persists normalized session history and encrypted terminal
scrollback while processes remain active. Terminal segments use
XChaCha20-Poly1305 with a domain-separated key derived from the application
database key and are pruned to 10 MiB per terminal. SQLCipher remains the
authority for segment metadata and session indexes.

OSC clipboard/title behavior is blocked or isolated in the terminal surface.
Only credential-free HTTP(S) hyperlinks are eligible for a native confirmation
dialog, and approved links are opened through fixed platform executables
without a shell. GUI actions never create a second process path: they are sent
to the same daemon-owned CLI or PTY session and reflected in its event stream.
