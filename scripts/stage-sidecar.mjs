#!/usr/bin/env node

import { existsSync } from "node:fs";
import { chmod, copyFile, mkdir } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = join(scriptDirectory, "..");
const configuredTarget = process.env.TAURI_ENV_TARGET_TRIPLE?.trim();
const target = configuredTarget || hostTarget();
const cargoExecutable = rustTool("cargo");
const debug = process.argv.includes("--debug");
const unknownArguments = process.argv.slice(2).filter((argument) => argument !== "--debug");
if (unknownArguments.length > 0) {
  throw new Error(`Unknown stage-sidecar argument: ${unknownArguments[0]}`);
}

const sidecars = ["maestrod", "maestro-fake-agent"];
const cargoArguments = [
  "build",
  "--locked",
  ...(debug ? [] : ["--release"]),
  ...sidecars.flatMap((sidecar) => ["--package", sidecar]),
  "--target",
  target,
];
const cargo = spawnSync(
  cargoExecutable,
  cargoArguments,
  { cwd: repositoryRoot, stdio: "inherit" },
);
if (cargo.error) throw cargo.error;
if (cargo.status !== 0) process.exit(cargo.status ?? 1);

const destinationDirectory = join(repositoryRoot, "apps", "desktop", "src-tauri", "binaries");
await mkdir(destinationDirectory, { recursive: true });
for (const sidecar of sidecars) {
  const source = join(repositoryRoot, "target", target, debug ? "debug" : "release", sidecar);
  const destination = join(destinationDirectory, `${sidecar}-${target}`);
  await copyFile(source, destination);
  await chmod(destination, 0o755);
}
console.log(
  `Staged Maestro ${debug ? "debug" : "release"} daemon and fake-agent sidecars for ${target}.`,
);

function hostTarget() {
  const rustc = spawnSync(rustTool("rustc"), ["--print", "host-tuple"], { encoding: "utf8" });
  if (rustc.error) throw rustc.error;
  if (rustc.status !== 0) process.exit(rustc.status ?? 1);
  const value = rustc.stdout.trim();
  if (!value) throw new Error("rustc did not report a host target tuple");
  return value;
}

function rustTool(name) {
  const configured = process.env[name.toUpperCase()]?.trim();
  if (configured) return configured;
  const cargoHome = process.env.CARGO_HOME?.trim();
  const executable = process.platform === "win32" ? `${name}.exe` : name;
  const candidates = [
    cargoHome ? join(cargoHome, "bin", executable) : null,
    join(homedir(), ".cargo", "bin", executable),
  ];
  return candidates.find((candidate) => candidate && existsSync(candidate)) ?? executable;
}
