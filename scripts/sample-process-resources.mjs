#!/usr/bin/env node

import { execFile } from "node:child_process";
import { writeFile } from "node:fs/promises";
import { arch, cpus, platform, release, totalmem } from "node:os";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const options = parseArguments(process.argv.slice(2));
const startedAt = new Date();
const deadline = Date.now() + options.durationMilliseconds;
const hostCpus = cpus();
const samples = [];

while (Date.now() <= deadline) {
  const capturedAt = new Date();
  const processes = await sampleProcesses(options.pids);
  samples.push({
    capturedAt: capturedAt.toISOString(),
    cpuPercent: sum(processes.map((process) => process.cpuPercent)),
    processes,
    rssBytes: sum(processes.map((process) => process.rssBytes)),
  });
  if (Date.now() >= deadline) break;
  await delay(Math.min(options.intervalMilliseconds, deadline - Date.now()));
}

const populated = samples.filter((sample) => sample.processes.length > 0);
if (populated.length === 0) {
  console.error("No requested process was present during the sampling window.");
  process.exit(66);
}

const summary = {
  averageCpuPercent: round(average(populated.map((sample) => sample.cpuPercent))),
  maximumCpuPercent: round(Math.max(...populated.map((sample) => sample.cpuPercent))),
  maximumRssBytes: Math.max(...populated.map((sample) => sample.rssBytes)),
  maximumRssMiB: round(
    Math.max(...populated.map((sample) => sample.rssBytes)) / (1024 * 1024),
  ),
  samplesWithProcesses: populated.length,
  totalSamples: samples.length,
};
const report = {
  schemaVersion: 1,
  label: options.label,
  host: {
    architecture: arch(),
    cpuModel: hostCpus.at(0)?.model ?? "unknown",
    logicalCpuCount: hostCpus.length,
    memoryBytes: totalmem(),
    os: platform(),
    osRelease: release(),
  },
  requestedPids: options.pids,
  startedAt: startedAt.toISOString(),
  endedAt: new Date().toISOString(),
  intervalMilliseconds: options.intervalMilliseconds,
  summary,
  samples,
};

if (options.output) {
  await writeFile(options.output, `${JSON.stringify(report, null, 2)}\n`, {
    encoding: "utf8",
    flag: "wx",
  });
  console.log(`Wrote raw samples to ${options.output}`);
}
console.log(JSON.stringify({ label: options.label, summary }, null, 2));

let thresholdFailed = false;
if (options.maximumRssMiB !== undefined && summary.maximumRssMiB > options.maximumRssMiB) {
  console.error(
    `Maximum RSS ${summary.maximumRssMiB} MiB exceeded ${options.maximumRssMiB} MiB.`,
  );
  thresholdFailed = true;
}
if (
  options.maximumAverageCpuPercent !== undefined &&
  summary.averageCpuPercent > options.maximumAverageCpuPercent
) {
  console.error(
    `Average CPU ${summary.averageCpuPercent}% exceeded ${options.maximumAverageCpuPercent}%.`,
  );
  thresholdFailed = true;
}
if (thresholdFailed) process.exitCode = 1;

function parseArguments(arguments_) {
  const parsed = {
    durationMilliseconds: 300_000,
    intervalMilliseconds: 1_000,
    label: "Maestro process sample",
    pids: [],
  };
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    const value = arguments_[index + 1];
    if (argument === "--pid") {
      requireValue(argument, value);
      parsed.pids.push(...value.split(",").map(parsePid));
      index += 1;
    } else if (argument === "--duration-seconds") {
      requireValue(argument, value);
      parsed.durationMilliseconds = parsePositive(value, argument) * 1_000;
      index += 1;
    } else if (argument === "--interval-milliseconds") {
      requireValue(argument, value);
      parsed.intervalMilliseconds = parsePositive(value, argument);
      index += 1;
    } else if (argument === "--label") {
      requireValue(argument, value);
      parsed.label = value;
      index += 1;
    } else if (argument === "--output") {
      requireValue(argument, value);
      parsed.output = value;
      index += 1;
    } else if (argument === "--max-rss-mib") {
      requireValue(argument, value);
      parsed.maximumRssMiB = parsePositive(value, argument);
      index += 1;
    } else if (argument === "--max-average-cpu-percent") {
      requireValue(argument, value);
      parsed.maximumAverageCpuPercent = parsePositive(value, argument);
      index += 1;
    } else {
      fail(`unknown argument: ${argument}`);
    }
  }
  parsed.pids = [...new Set(parsed.pids)];
  if (parsed.pids.length === 0) fail("at least one --pid is required");
  if (parsed.intervalMilliseconds > parsed.durationMilliseconds) {
    fail("sampling interval cannot exceed duration");
  }
  return parsed;
}

async function sampleProcesses(pids) {
  try {
    const { stdout } = await execFileAsync(
      "/bin/ps",
      ["-o", "pid=,rss=,%cpu=", "-p", pids.join(",")],
      { env: { ...process.env, LANG: "C", LC_ALL: "C" } },
    );
    return stdout
      .trim()
      .split(/\r?\n/u)
      .filter(Boolean)
      .map((line) => {
        const [pid, rssKiB, cpuPercent] = line.trim().split(/\s+/u);
        return {
          cpuPercent: Number.parseFloat(cpuPercent),
          pid: Number.parseInt(pid, 10),
          rssBytes: Number.parseInt(rssKiB, 10) * 1024,
        };
      })
      .filter(
        (process) =>
          Number.isFinite(process.pid) &&
          Number.isFinite(process.rssBytes) &&
          Number.isFinite(process.cpuPercent),
      );
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === 1) return [];
    throw error;
  }
}

function parsePid(value) {
  const pid = Number.parseInt(value, 10);
  if (!Number.isSafeInteger(pid) || pid <= 0 || String(pid) !== value.trim()) {
    fail(`invalid process ID: ${value}`);
  }
  return pid;
}

function parsePositive(value, option) {
  const number = Number(value);
  if (!Number.isFinite(number) || number <= 0) fail(`${option} must be positive`);
  return number;
}

function requireValue(option, value) {
  if (value === undefined || value.startsWith("--")) fail(`${option} requires a value`);
}

function fail(message) {
  console.error(message);
  console.error(
    "usage: node scripts/sample-process-resources.mjs --pid <pid[,pid...]> [--duration-seconds 300] [--interval-milliseconds 1000] [--output report.json]",
  );
  process.exit(64);
}

function sum(values) {
  return values.reduce((total, value) => total + value, 0);
}

function average(values) {
  return sum(values) / values.length;
}

function round(value) {
  return Math.round(value * 100) / 100;
}

function delay(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}
