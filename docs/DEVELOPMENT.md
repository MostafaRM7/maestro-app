# Maestro development guide

Maestro is a Tauri 2 desktop application with a Rust workspace and a
pnpm-managed TypeScript workspace. The application targets macOS 13+ on ARM64
and Intel, plus Ubuntu 22.04 x86_64. Ubuntu ARM64 is not a release target.

This guide covers the Foundation toolchain and validation workflow. It does not
configure signing, notarization, publishing, or vendor CLI credentials.

## Toolchain policy

- Rust is pinned to 1.97.1 by `rust-toolchain.toml`. `rustup` installs the
  minimal profile together with rustfmt and Clippy.
- Node.js 22.23.1 is used in CI. Local Node.js 22 installations should use the
  pnpm version declared by the root `package.json` `packageManager` field.
- `Cargo.lock` and `pnpm-lock.yaml` are application lockfiles and must be
  committed. CI installs from them without updating dependency resolution.
- GitHub Actions are pinned to full commit hashes. The adjacent version comments
  are informational and make deliberate upgrades reviewable.

Do not install Codex, Claude Code, or `agy` as JavaScript or Rust dependencies.
Maestro discovers and invokes the user's existing executables. Live tests that
consume vendor resources remain opt-in.

## Platform prerequisites

### macOS 13+

Install:

- Xcode Command Line Tools (`xcode-select --install`)
- `rustup`
- Node.js 22
- pnpm through Corepack, using the version declared in `package.json`
- `pkg-config` and SQLCipher development libraries

Both Apple Silicon and Intel builds are native CI jobs. Cross-compiling an Intel
artifact on an Apple Silicon development machine is not a substitute for the
native Intel CI result.

### Ubuntu 22.04 x86_64

Install the compiler, Tauri/WebKit, Secret Service, SQLCipher, and packaging
prerequisites:

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends \
  build-essential \
  libayatana-appindicator3-dev \
  libdbus-1-dev \
  libsecret-1-dev \
  libsqlcipher-dev \
  libssl-dev \
  libwebkit2gtk-4.1-dev \
  librsvg2-dev \
  patchelf \
  pkg-config
```

Install `rustup`, Node.js 22, and then activate the repository's pinned pnpm
version with Corepack. Follow the official installation instructions for these
tools rather than piping unreviewed third-party installation scripts into a
shell.

Ubuntu validation must cover both Wayland and X11 before a release. The ordinary
headless checks in CI do not replace those desktop smoke tests.

## Bootstrap

From the repository root:

```sh
corepack enable
corepack install
pnpm install --frozen-lockfile
rustup show active-toolchain
```

If Corepack is not included in the selected Node.js distribution, install
Corepack from its official package first; do not choose an arbitrary global pnpm
version. The root `packageManager` declaration remains the source of truth.

No vendor CLI is required for deterministic Foundation tests. Fake CLI fixtures
must be used by default.

## Validation

Run the same required checks as CI from the repository root:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
pnpm lint
pnpm typecheck
pnpm test
pnpm build
```

Dependency and license policy checks are:

```sh
cargo deny --all-features check
pnpm audit --audit-level high
```

`cargo-deny` is configured by `deny.toml`. It rejects vulnerabilities, yanked
crates, unknown registries, unknown Git sources, unlicensed crates, and licenses
outside the explicit permissive allowlist. Multiple crate versions are warnings
during Foundation and should be reviewed rather than ignored. The only broadly
allowed legacy identifier is `Unicode-DFS-2016`, retained for older Unicode
transitive crates; crate-specific exceptions must include a documented reason.

## Running the desktop app

The root scripts delegate to the `@maestro/desktop` workspace:

```sh
pnpm dev
pnpm tauri dev
```

The Tauri configuration lives at
`apps/desktop/src-tauri/tauri.conf.json`. Runtime state, encrypted databases,
logs, and support bundles must never be committed.

## Native package smoke builds

Local unsigned smoke packages can be produced with:

```sh
pnpm tauri build --target aarch64-apple-darwin --bundles dmg
pnpm tauri build --target x86_64-apple-darwin --bundles dmg
pnpm tauri build --target x86_64-unknown-linux-gnu --bundles appimage,deb
```

Only run the command matching the native host architecture. GitHub Actions also
exposes a manual `package_native` workflow input that builds all three unsigned
targets. Those artifacts are retained for seven days for engineering smoke
tests and are not release artifacts.

Foundation CI intentionally has no signing, notarization, updater, release, or
publishing permissions. A later protected release workflow must use GitHub
environments, scoped secrets, signed update manifests, provenance, and explicit
human approval.

## CI jobs

- `rust-quality`: formatting, Clippy, and workspace tests on Ubuntu 22.04.
- `frontend-quality`: lint, type checking, tests, and production web build.
- `dependency-policy`: Cargo advisory/license/source policy and pnpm audit.
- `dependency-review`: reviews dependency changes on pull requests.
- `native-check`: native Rust compilation and tests on macOS ARM64, macOS
  Intel, and Ubuntu 22.04 x86_64.
- `package-native`: manual-only unsigned DMG, AppImage, and `.deb` smoke builds.

CI receives a read-only `GITHUB_TOKEN`, checkout does not persist credentials,
and third-party actions are immutable SHA pins. Pull requests never receive
packaging or release credentials.

## Security boundaries for development

- Never add provider SDKs or direct provider HTTP calls. Agent/provider traffic
  must go through the actual CLI executable.
- Never copy vendor tokens into Maestro configuration, test fixtures, logs, or
  GitHub secrets.
- Use argument arrays for child processes; do not interpolate commands into a
  shell string.
- Keep `.env` loading opt-in and project-scoped. `.env` files are ignored; only
  a redacted `.env.example` may be committed.
- Test data must contain synthetic secrets so redaction tests cannot disclose
  usable credentials.
- Do not enable telemetry in tests or development by default.
- Treat manually generated unsigned packages as untrusted engineering output,
  not distributable releases.

## Before opening a pull request

1. Run formatting, linting, type checking, tests, and dependency policy checks.
2. Confirm generated files, databases, logs, credentials, and signing material
   are absent from the diff.
3. Document any new environment variable, migration, permission, network
   destination, or external executable requirement.
4. Mark live CLI tests clearly and keep them out of the default suite.
5. Verify that unsupported platform or CLI capabilities remain explicit rather
   than silently falling back.
