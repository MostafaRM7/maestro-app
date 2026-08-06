# Milestone 0 security and performance evidence

Status: automated boundary checks implemented; release-candidate runtime and
performance measurements remain open  
Audit date: 2026-08-06

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
The first GitHub Actions result for the candidate commit must be linked here
before the milestone is signed off; the workflow definition alone is not a
recorded pass.

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

## Evidence matrix and open gates

| Gate | Current automated evidence | Remaining release evidence |
|---|---|---|
| NET-001 provider boundary | Source/manifest/lock scan is local-pass and CI-gated | Review scan rule updates whenever supported vendors or dependency formats change |
| NET-002 no-network tests | Route-free namespace job is defined for Rust and frontend suites | Link a green candidate-SHA CI run |
| NET-003 local IPC only | Unix socket tests/source guard plus one optimized macOS ARM64 startup capture with no daemon Internet socket | Capture packaged common workflows on macOS ARM64, macOS x86_64, and Ubuntu x86_64 |
| PERF-DAEMON-001 | One five-minute optimized idle run: 15.34 MiB max RSS, 0% average CPU | Three release normal-workload runs and worst-case result against ~50 MiB daemon target |
| PERF-GUI-001 / PERF-IDLE-001 | One five-minute optimized idle run: 203.16 MiB max RSS, 0.14% average CPU, no sustained >2% interval | Three defined normal-workload runs against ~250 MiB GUI and idle-CPU targets |
| PERF-START-001 | None | Ten cold candidate launches per reference target; record input-ready p95 |
| PERF-CONC-001 | Process/session concurrency and fake fixtures have automated functional coverage | Ten-session packaged workload with loss/cross-talk/resource evidence |
| PERF-FLOOD-001 | Deterministic flood fixture and bounded channels/storage tests exist | Five-minute packaged flood plus interactive-session responsiveness evidence |
| PERF-LEAK-001 | Lifecycle/process-tree tests exist | Twenty-cycle release-build handle/task/process/RSS measurements |
| PERF-REPO-001 | Bounded/lazy project operations have automated coverage | Generated ~100,000-file repository measurement and cancellation latency |

No critical or high finding was identified in this scoped provider/network
audit. Milestone 0 must not be called complete until the candidate CI link and
the remaining platform/runtime/performance evidence above are recorded or the
corresponding gate is explicitly returned to design review.

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
