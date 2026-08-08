# Maestro

Maestro is a local-first desktop control center for installed AI coding-agent
CLIs. It provides a unified graphical workflow for Codex CLI, Claude Code CLI,
and Antigravity CLI (`agy`) while keeping every provider interaction inside the
official executable.

The repository is in the Foundation milestone. The approved product and system
design is in [MAESTRO_ARCHITECTURE.md](MAESTRO_ARCHITECTURE.md).

## Initial targets

- macOS 13+ on Apple Silicon
- Ubuntu 22.04+ on x86_64 (Wayland in Foundation; X11 validation in Milestone 4)

## Repository layout

- `apps/desktop`: Tauri 2 desktop host and React interface
- `crates/maestrod`: per-user daemon
- `crates/maestro-domain`: durable domain and event types
- `crates/maestro-protocol`: authenticated local IPC protocol
- `crates/maestro-process`: controlled process and PTY primitives
- `crates/maestro-storage`: encrypted SQLCipher persistence
- `fixtures/fake-agent`: deterministic CLI used in tests

## Security boundary

Maestro does not call AI-provider APIs, import provider SDKs, or store vendor
credentials. The installed CLIs remain the source of truth for authentication,
configuration, and provider communication.

## Run the Foundation build locally

Prerequisites are Node.js 22+, `pnpm` 11+, and the Rust toolchain. `corepack` is
not required when `pnpm` is already installed.

```sh
pnpm install --frozen-lockfile
pnpm dev
```

The development command builds and stages matching debug daemon and fake-agent
sidecars, then opens the native Maestro window through Tauri. Milestone 0 uses
only the local deterministic fake agent; real Codex, Claude Code, and `agy`
adapters begin in later milestones.

## License

MIT
