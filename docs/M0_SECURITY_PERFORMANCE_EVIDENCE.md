# Milestone 0 security and performance evidence

Status: automated boundary checks implemented; release-candidate runtime and
performance measurements remain open  
Audit date: 2026-08-08

This record covers the provider/network boundary, daemon transport, and
resource targets for the Foundation milestone. It distinguishes repeatable
automated evidence from platform evidence that still has to be collected from
an optimized desktop build.

## Automated security boundary

`scripts/verify-m0-boundaries.mjs` is a deterministic source and dependency
guard. It fails when it finds:

- a known OpenAI, Anthropic, or Google AI provider SDK in Rust or JavaScript
  manifests/lockfiles;
- a direct OpenAI, Anthropic, or Google AI provider endpoint in product source;
- TCP or UDP transport primitives in the first-party daemon source; or
- removal of the daemon's required `UnixListener`/`UnixStream` anchors.

The scan deliberately does not ban generic URL handling or all networking
dependencies. Maestro may eventually contact its own signed update service or
an adapter catalog, while agent/provider communication remains forbidden
outside installed CLI processes.

Local result on the audited tree:

```text
M0 boundaries verified: no direct provider SDK/endpoint and no daemon TCP/UDP transport detected; Unix-domain IPC anchors are present.
```

The same tree passes `pnpm audit --audit-level high` with no known
vulnerabilities. A local `cargo deny` executable was not available, so the
pinned `cargo-deny-action` CI job remains the recorded Rust advisory, license,
ban, and source-policy gate.

The implementation evidence behind the transport assertion is:

- `crates/maestrod/src/server.rs` owns a `tokio::net::UnixListener`, binds it at
  the per-user socket path, and accepts `UnixStream` clients.
- `crates/maestrod/src/ipc.rs` connects through `tokio::net::UnixStream`.
- daemon integration tests bind isolated Unix sockets and exercise authenticated
  request/response traffic, single-instance behavior, pre-authentication
  limits, and cleanup.
- the boundary scan rejects introduction of first-party daemon TCP/UDP types.

The Tauri development URL and `http://ipc.localhost` CSP token are webview/Tauri
development and custom-IPC mechanisms. They are not the daemon transport and
are not provider endpoints.

## Network-denied deterministic CI

The `network-denied-tests` CI job installs locked dependencies and compiles test
executables before isolation. It then runs both deterministic suites inside a
fresh Linux network namespace:

1. `unshare --net --mount-proc` creates the namespace with only a loopback
   interface and no external route.
   The privileged helper creates the namespace and then `setpriv` drops back to
   the ordinary GitHub runner UID/GID before any repository test executes.
2. `scripts/run-without-network.sh` refuses to proceed unless the namespace is
   distinct from the host namespace, contains no non-loopback interface, and
   has no default IPv4 route.
3. Rust tests run with both `CARGO_NET_OFFLINE=true` and Cargo's `--offline`
   option, using only the local fake-agent executable.
4. Frontend tests run in a second route-free namespace using the already
   installed locked dependencies.

Dependency acquisition is intentionally outside the network-denied phase. The
test execution itself cannot make an outbound provider or other network call.
The first GitHub Actions result for the candidate commit is recorded in the
"Ubuntu 22.04 CI build and package evidence" section below; the workflow
definition alone is not a recorded pass.

## Runtime socket evidence still required

The source guard and authenticated Unix-socket integration tests cover the
first-party transport implementation. They do not independently observe all
file descriptors opened by a packaged Tauri/webview process or its transitive
native libraries.

For each release-candidate platform, retain a sanitized socket capture while
performing first launch, project open, fake structured session, fake TUI, and
ordinary shell terminal flows. The capture must show:

- `maestrod` listening only on its owner-restricted Unix-domain socket;
- no Maestro or `maestrod` TCP listener;
- no outbound OpenAI, Anthropic, or Google AI provider connection; and
- the exact candidate commit, package architecture, OS, and capture tool.

Do not use a live vendor CLI in this Foundation capture. A live CLI would make
provider traffic expected in the CLI process and would obscure the application
boundary being tested.

## Resource measurement harness

`scripts/sample-process-resources.mjs` samples one or more explicit PIDs with
the platform `ps` executable. It records per-process and aggregate RSS/CPU data
without recording command lines, environments, file paths, or process output.
It runs on macOS and Linux, includes the OS/architecture/CPU/RAM measurement
context, and refuses to overwrite an existing report.

Example daemon measurement after the required settle period:

```sh
node scripts/sample-process-resources.mjs \
  --pid 12345 \
  --duration-seconds 300 \
  --interval-milliseconds 1000 \
  --max-rss-mib 50 \
  --label "maestrod idle candidate" \
  --output daemon-resources.json
```

For GUI measurements, pass all host and webview PIDs with repeated `--pid`
options or one comma-separated value. Child agent/shell PIDs must be measured
separately and excluded from the daemon/GUI budget. Store sanitized raw JSON as
release evidence rather than committing machine-specific samples to the source
tree.

The sampler covers steady RSS and average CPU. It does not measure first-paint
or input-ready time, event integrity, UI latency, process/task leaks, or file
tree behavior; those require their workload-specific procedures in the M0 test
plan.

A five-second local smoke run against the debug `maestrod` executable produced
10 samples with 0% average/maximum CPU and 9.34 MiB maximum RSS, followed by a
clean daemon termination. This validates the sampler and is encouraging, but it
is not PERF-DAEMON-001 evidence: that gate requires the release workload, a
60-second settle period, five minutes of sampling, and three recorded runs.

On 2026-08-06, one optimized macOS ARM64 idle run on an Apple M4 sampled the
release daemon and the desktop/WebKit aggregate for five minutes after Keychain
approval and singleton convergence. The daemon recorded 15.34 MiB maximum RSS,
0% average CPU, and 0.1% maximum CPU. The desktop/WebKit aggregate recorded
203.16 MiB maximum RSS and 0.14% average CPU. Its only greater-than-2% burst was
four consecutive one-second samples, below the ten-second sustained-idle limit.
The sanitized raw reports are retained locally under `target/m0-evidence/` and
remain intentionally ignored by Git. This is one valid idle release sample,
not formal closure of the performance gates: it did not include the defined
normal project/session workload and the test plan requires three runs.

The same launch converged to one `maestrod` process. A local `lsof` observation
showed the owner-scoped `maestrod.sock` Unix listener and no Internet socket for
the desktop host, its WebKit helpers, or that daemon. This is useful macOS ARM64
startup evidence, but it does not replace the packaged common-workflow capture
required by `NET-003`/`MAN-NET-001`.

## macOS ARM64 package and secure-store smoke

On 2026-08-06, `pnpm tauri build --bundles app` produced an optimized ARM64
`Maestro.app` containing the desktop executable plus both Foundation sidecars,
`maestrod` and `maestro-fake-agent`, under `Contents/MacOS`. The configured
`icon.icns` was present under `Contents/Resources`.

Tauri's local unsigned build retained only the Mach-O linker signature, which
does not seal bundle resources and therefore does not pass a strict whole-bundle
`codesign` verification. Applying a local ad-hoc deep signature to the generated
artifact made `codesign --verify --deep --strict` pass and reported the icon and
sidecars as sealed resources. This validates the local bundle structure only;
it is not release signing, notarization, or distribution evidence.

After the user approved the native Keychain prompt, a metadata-only
`security find-generic-password` lookup confirmed a Keychain item with service
`com.maestroai.app` and account `database-key-v1`. No secret value was requested
or printed. This is useful local `MAN-MAC-001` evidence, but the complete
release-candidate first-launch/relaunch checklist still remains open.

## Ubuntu x86_64 package and accepted Wayland smoke

On 2026-08-08, the Foundation matrix ran locally on an x86_64 Zorin OS 18.1
host (`ID_LIKE="ubuntu debian"`, Ubuntu codename Noble), GNOME Wayland, and
glibc 2.39. By product decision, this Ubuntu-family host is accepted as the
Milestone 0 Ubuntu Wayland environment. The following checks passed:

- Rust formatting and strict workspace Clippy;
- 213 Rust tests, with two explicit subprocess helpers intentionally ignored;
- 116 frontend tests, TypeScript checking, ESLint, and production web build;
- native `x86_64-unknown-linux-gnu` compilation;
- the provider/IPC boundary scan and `pnpm audit --audit-level high`; and
- optimized AppImage and `.deb` packaging with both Foundation sidecars.

The generated packages were:

```text
target/x86_64-unknown-linux-gnu/release/bundle/appimage/Maestro_0.1.0_amd64.AppImage
  SHA-256 7ba62dbafe3853b0d8cf9892cacf0d3722a246cefa50e2b3da91b430dc18a2c9
target/x86_64-unknown-linux-gnu/release/bundle/deb/Maestro_0.1.0_amd64.deb
  SHA-256 29789e6623fb74e7d8f34690b540dd72cd38bb36eb85c8faaef3a34b25d6e38c
```

The Debian metadata reports `Architecture: amd64`. Package inspection found
`maestro-desktop`, `maestrod`, and `maestro-fake-agent` in both formats, and all
three are x86-64 ELF executables. Because the packages were built on Noble,
their binaries require symbols available through glibc 2.39; they do not prove
the Ubuntu 22.04 minimum baseline. The Ubuntu 22.04 native packaging job remains
the authoritative compatibility gate.

The AppImage also copied the unrelated host multiarch module
`usr/lib/i386-linux-gnu/gio/modules/libgiognutls.so`. The three Maestro
executables are x86_64, but this 32-bit host contamination must be removed or
explicitly dispositioned before treating the AppImage as a release candidate.

For the runtime smoke, the AppImage was launched on the real Wayland display
with isolated temporary XDG data/config/cache/runtime roots and an isolated
D-Bus session. Secret Service activation on that new bus timed out, exercising
the unavailable-service startup path. The desktop and WebKit helpers remained
alive, and startup eventually converged to one `maestrod`. The runtime created
an authentication token and `maestrod.sock`, both mode `0600`, under the
isolated runtime root. No application database was created before interactive
passphrase setup. `lsof` showed no Internet socket for the desktop or daemon,
and the host TCP/UDP listener capture attributed no listener to a Maestro
process. Interrupting the isolated launch exited the desktop, daemon, WebKit,
and D-Bus process tree without leaving a `maestro-desktop` or `maestrod`
process.

Several daemon attempts occurred while Secret Service activation was timing
out before one daemon remained stable. The isolated launch did not complete
interactive passphrase creation/relaunch, so this observation is not a
`MAN-LNX-002` pass and must be repeated with a release candidate. The full
Wayland desktop workflow (`MAN-LNX-003`), Secret Service available path
(`MAN-LNX-001`), and Ubuntu 22.04 launch/build smoke (`MAN-LNX-005`) remain
open. X11 desktop parity is deferred to `M4-LNX-X11-001` and no longer blocks
Milestone 0.

## Ubuntu 22.04 CI build and package evidence

Candidate commit `0950275c329d2ddf0d8f33ae4b529e8ff9a36206` produced two green
GitHub Actions runs.

The push run
(https://github.com/MostafaRM7/maestro-app/actions/runs/31255203107) passed
`Rust quality`, `Frontend quality`, `Dependency and license policy`,
`Deterministic tests without network` (route-free namespaces), `Native test
(macOS ARM64)`, and `Native test (Ubuntu 22.04 x86_64)`. The PR-only `Pull
request dependency review` job and the workflow-dispatch-only `Unsigned
package` matrix job were skipped as expected for that event type.

The manual package run
(https://github.com/MostafaRM7/maestro-app/actions/runs/31255743665) was
dispatched with `package_native=true` at the same commit and passed every
applicable job, including `Unsigned package (Ubuntu 22.04 x86_64)` and
`Unsigned package (macOS ARM64)`. Ubuntu packaging ran on the Ubuntu 22.04
runner, which preserves the minimum glibc baseline.

Downloaded Ubuntu artifact files and SHA-256 hashes:

```text
maestro-ubuntu-x86_64-unsigned.zip       250227644 bytes 16e969d628e67849d8ca41bd9fb51abcecbc591bb9052078d60503d7bb90af1d
Maestro_0.1.0_amd64.deb                  15520828  bytes fc8973be4792122e0a7ba1bd296f55e105649b25d39fc1df39a35e528c1394d2
Maestro_0.1.0_amd64.AppImage             91625976  bytes 940c040916dc6527ab9844988beaf77e972c0bb951d970f2be0f27531487c09e
```

The macOS ARM64 package job succeeded and its artifact
`maestro-macos-arm64-unsigned` is listed in the run's artifact API (13,357,569
bytes). Its content was not downloaded in this phase; macOS download/hash
inspection is deferred.

The `.deb` control metadata reports `Package: maestro`, `Version: 0.1.0`, and
`Architecture: amd64`, with `Depends: libwebkit2gtk-4.1-0, libgtk-3-0`. Its
`usr/bin` ships `maestro-desktop`, `maestrod`, and `maestro-fake-agent`. The
AppImage outer file is `ELF 64-bit LSB pie executable, x86-64, static-pie
linked, stripped`; its embedded zstd SquashFS (324 inodes) contains the same
three executables plus `AppRun`, `AppRun.wrapped`, and `Maestro.desktop`.

All six shipped Maestro executables (three per package format) are `ELF 64-bit`
x86-64 dynamically linked PIE binaries, each with an identical BuildID across
both formats:

| Executable | BuildID |
|---|---|
| `maestro-desktop` | `dc9b5a5ac79056cfb0eb85296cd4ec7134dadfa0` |
| `maestrod` | `e68fd1ad1b24ceaa58828f869b862f3ceafbaec2` |
| `maestro-fake-agent` | `7f33466dae5adf6cd330cc6a7145ea3ee0588810` |

ELF architecture scan across both package trees (3 ELF files in the `.deb`;
in the AppImage's extracted SquashFS, 172 unique ELF regular files plus 30
symlink aliases to ELF targets, for 202 dereferenced ELF paths). Symlinks are
distinct from regular files: the 30 symlink aliases (e.g.
`usr/lib/libgio-2.0.so`) resolve to regular ELF targets, while non-ELF symlinks
(`.DirIcon`, `Maestro.desktop`, `maestro-desktop.png`) point at icons/metadata.
No `ELF 32-bit`, `i386`, or `Intel 80386` file and no `i386-linux-gnu` path
were found. The 32-bit GIO module contamination seen in the earlier local
Noble AppImage is absent here; the CI AppImage bundles the x86-64 module
`usr/lib/x86_64-linux-gnu/gio/modules/libgiognutls.so` instead.

The outer AppImage runtime is itself an `ELF 64-bit LSB pie executable,
x86-64, static-pie linked, stripped` file. Being static PIE, it carries no
dynamic symbol version information and therefore has no dynamic GLIBC
requirement; it is excluded from the GLIBC ceiling table below.

GLIBC symbol-version ceilings measured with `objdump -T` on every shipped ELF
file:

| Scope | Maximum required GLIBC |
|---|---|
| Each of the six Maestro executables | `GLIBC_2.34` |
| `.deb` package, all shipped ELF | `GLIBC_2.34` |
| AppImage, all shipped ELF | `GLIBC_2.35` (bundled `libwebkit2gtk-4.1.so.0`) |

The maximum requirement is at or below the Ubuntu 22.04 baseline of
`GLIBC_2.35`, so the packages do not depend on a newer glibc baseline. This is
Ubuntu 22.04 build/package evidence only; it is not completion of the
interactive Wayland, Secret Service, passphrase, or performance gates, and it
makes no X11 claim. The workspace packages are all configured non-publishable
(`publish = false`) as part of the cargo-deny wildcard-path policy.

## Remaining Milestone 0 gates after the platform decision

- Re-run the green candidate-SHA GitHub Actions push and manual package runs
  after any future candidate change; the recorded runs are for commit
  `0950275c329d2ddf0d8f33ae4b529e8ff9a36206`.
- Complete the Ubuntu Wayland manual checklist: Secret Service available
  first-launch/relaunch, unavailable-service passphrase create/wrong/unlock/
  relaunch, tray and multi-window lifecycle, theme/scaling, clipboard and
  terminal input, file/Git/external-editor workflows, redaction, and a full
  runtime socket capture on the Ubuntu 22.04 CI package.
- Complete the corresponding packaged macOS ARM64 common workflows and retain
  the full Keychain, menu-bar, terminal/TUI, multi-window, lifecycle, redaction,
  and runtime-network evidence.
- Run the defined three-run performance matrix: startup p95, daemon/GUI RSS,
  idle CPU, ten-session concurrency, flood responsiveness, lifecycle leak,
  retention, and the approximately 100,000-file repository workload.
- Finish the security review with no unresolved critical/high issue, release
  blocker, or flaky required gate; the repeated daemon attempts observed during
  the isolated local Secret Service timeout remain open for release-candidate
  repetition.

## Evidence matrix and open gates

| Gate | Current automated evidence | Remaining release evidence |
|---|---|---|
| NET-001 provider boundary | Source/manifest/lock scan is local-pass and CI-gated | Review scan rule updates whenever supported vendors or dependency formats change |
| NET-002 no-network tests | Green candidate-SHA runs (push 31255203107, package 31255743665) executed both suites in route-free namespaces with namespace-identity proof | Re-run on any future candidate commit |
| NET-003 local IPC only | Unix socket tests/source guard, one optimized macOS ARM64 startup capture, and one isolated Ubuntu-family Wayland startup capture; neither desktop nor daemon opened an Internet socket | Capture complete packaged common workflows on macOS ARM64 and Ubuntu x86_64 |
| PERF-DAEMON-001 | One five-minute optimized idle run: 15.34 MiB max RSS, 0% average CPU | Three release normal-workload runs and worst-case result against ~50 MiB daemon target |
| PERF-GUI-001 / PERF-IDLE-001 | One five-minute optimized idle run: 203.16 MiB max RSS, 0.14% average CPU, no sustained >2% interval | Three defined normal-workload runs against ~250 MiB GUI and idle-CPU targets |
| PERF-START-001 | None | Ten cold candidate launches per reference target; record input-ready p95 |
| PERF-CONC-001 | Process/session concurrency and fake fixtures have automated functional coverage | Ten-session packaged workload with loss/cross-talk/resource evidence |
| PERF-FLOOD-001 | Deterministic flood fixture and bounded channels/storage tests exist | Five-minute packaged flood plus interactive-session responsiveness evidence |
| PERF-LEAK-001 | Lifecycle/process-tree tests exist | Twenty-cycle release-build handle/task/process/RSS measurements |
| PERF-REPO-001 | Bounded/lazy project operations have automated coverage | Generated ~100,000-file repository measurement and cancellation latency |

No critical or high finding was identified in this scoped provider/network
audit. Milestone 0 must not be called complete until the remaining
platform/runtime/performance evidence above is recorded or the corresponding
gate is explicitly returned to design review.

## Local validation performed

```sh
node scripts/verify-m0-boundaries.mjs
bash -n scripts/run-without-network.sh
node --check scripts/verify-m0-boundaries.mjs
node --check scripts/sample-process-resources.mjs
node scripts/sample-process-resources.mjs --pid <shell-pid> --duration-seconds 1 --interval-milliseconds 250 --label sampler-smoke
```

The Linux network-namespace command cannot be executed on the macOS audit host;
its pass/fail evidence comes from the Ubuntu CI job.
