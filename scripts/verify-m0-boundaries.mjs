#!/usr/bin/env node

import { readdir, readFile } from "node:fs/promises";
import { dirname, extname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "..");
const findings = [];

const ignoredDirectories = new Set([
  ".git",
  ".codebase-memory",
  "node_modules",
  "target",
]);

const providerDependencyRules = [
  {
    label: "OpenAI provider SDK",
    pattern:
      /(?:^|[\n\r])\s*(?:async[-_]openai|openai(?:[-_]api)?|openai-api-rs|codex[-_]sdk)(?:\.workspace)?\s*=|["'](?:openai|@azure\/openai|@openai\/codex-sdk|codex-sdk)["']\s*:/imu,
  },
  {
    label: "Anthropic provider SDK",
    pattern:
      /(?:^|[\n\r])\s*(?:anthropic|anthropic[-_]sdk|claude[-_]agent[-_]sdk)(?:\.workspace)?\s*=|["']@anthropic-ai\/(?:sdk|claude-agent-sdk)["']\s*:/imu,
  },
  {
    label: "Google AI provider SDK",
    pattern:
      /(?:^|[\n\r])\s*(?:google[-_]generative[-_]ai|gemini[-_]ai|genai|vertex[-_]?ai|google[-_]cloud[-_]aiplatform)(?:\.workspace)?\s*=|["'](?:@google\/generative-ai|@google\/genai|@google\/gemini-cli-core|@google-cloud\/vertexai)["']\s*:/imu,
  },
];

const providerLockRules = [
  {
    label: "OpenAI provider SDK in a lockfile",
    pattern:
      /(?:^name = "(?:async-openai|openai|openai-api-rs|codex-sdk)"$|^\s{2,}["']?(?:openai|@azure\/openai|@openai\/codex-sdk|codex-sdk)(?:@|:))/imu,
  },
  {
    label: "Anthropic provider SDK in a lockfile",
    pattern:
      /(?:^name = "(?:anthropic|anthropic-sdk|claude-agent-sdk)"$|^\s{2,}["']?@anthropic-ai\/(?:sdk|claude-agent-sdk)(?:@|:))/imu,
  },
  {
    label: "Google AI provider SDK in a lockfile",
    pattern:
      /(?:^name = "(?:google-generative-ai|gemini-ai|genai|vertexai|vertex-ai|google-cloud-aiplatform)"$|^\s{2,}["']?(?:@google\/generative-ai|@google\/genai|@google\/gemini-cli-core|@google-cloud\/vertexai)(?:@|:))/imu,
  },
];

const providerEndpointRules = [
  ["OpenAI provider endpoint", /(?:api\.openai\.com|\.openai\.azure\.com)/iu],
  ["Anthropic provider endpoint", /api\.anthropic\.com/iu],
  [
    "Google AI provider endpoint",
    /(?:generativelanguage|aiplatform)\.googleapis\.com/iu,
  ],
];

const daemonTcpRules = [
  ["TCP listener", /\b(?:TcpListener|tokio::net::TcpListener)\b/u],
  ["TCP stream", /\b(?:TcpStream|tokio::net::TcpStream)\b/u],
  ["UDP socket", /\b(?:UdpSocket|tokio::net::UdpSocket)\b/u],
  ["standard-library network transport", /\bstd::net::(?:Tcp|Udp)/u],
];

const sourceExtensions = new Set([".cjs", ".js", ".json", ".mjs", ".rs", ".ts", ".tsx"]);
const manifestNames = new Set(["Cargo.toml", "package.json"]);
const lockNames = new Set(["Cargo.lock", "pnpm-lock.yaml"]);

const allFiles = await collectFiles(repositoryRoot);
for (const absolutePath of allFiles) {
  const path = relative(repositoryRoot, absolutePath);
  const name = path.split("/").at(-1);
  const extension = extname(path);
  const isManifest = manifestNames.has(name);
  const isLockfile = lockNames.has(name);
  const isProductSource =
    sourceExtensions.has(extension) &&
    (path.startsWith("apps/") || path.startsWith("crates/"));

  if (!isManifest && !isLockfile && !isProductSource) continue;
  const content = await readFile(absolutePath, "utf8");

  if (isManifest) applyRules(path, content, providerDependencyRules);
  if (isLockfile) applyRules(path, content, providerLockRules);
  if (isProductSource) {
    applyRules(
      path,
      content,
      providerEndpointRules.map(([label, pattern]) => ({ label, pattern })),
    );
  }
  if (path.startsWith("crates/maestrod/src/") && extension === ".rs") {
    applyRules(
      path,
      content,
      daemonTcpRules.map(([label, pattern]) => ({ label, pattern })),
    );
  }
}

await requirePattern(
  "crates/maestrod/src/server.rs",
  /\bUnixListener\b/u,
  "daemon server must remain anchored to a Unix-domain listener",
);
await requirePattern(
  "crates/maestrod/src/ipc.rs",
  /\bUnixStream\b/u,
  "daemon client must remain anchored to a Unix-domain stream",
);

if (findings.length > 0) {
  console.error("Maestro M0 boundary verification failed:");
  for (const finding of findings) console.error(`- ${finding}`);
  process.exitCode = 1;
} else {
  console.log(
    "M0 boundaries verified: no direct provider SDK/endpoint and no daemon TCP/UDP transport detected; Unix-domain IPC anchors are present.",
  );
}

async function collectFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await collectFiles(path)));
    else if (entry.isFile()) files.push(path);
  }
  return files;
}

function applyRules(path, content, rules) {
  for (const { label, pattern } of rules) {
    const match = pattern.exec(content);
    if (!match) continue;
    const line = content.slice(0, match.index).split(/\r?\n/u).length;
    findings.push(`${path}:${line}: ${label}`);
  }
}

async function requirePattern(path, pattern, message) {
  const content = await readFile(join(repositoryRoot, path), "utf8");
  if (!pattern.test(content)) findings.push(`${path}: ${message}`);
}
