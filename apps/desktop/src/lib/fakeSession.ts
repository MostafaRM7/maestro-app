import { invoke } from "@tauri-apps/api/core";
import type { TerminalOpened } from "./terminalClient";

export type SessionState =
  | "created"
  | "starting"
  | "ready"
  | "running"
  | "awaiting_permission"
  | "awaiting_user_input"
  | "background"
  | "interrupting"
  | "completed"
  | "stopped"
  | "failed"
  | "interrupted"
  | "recoverable"
  | "incompatible";

export type EventSource = "cli" | "gui" | "daemon" | "hook" | "pty";
export type EventVisibility = "user" | "debug" | "sensitive";
export type PermissionDecision = "allow" | "deny" | "cancel";

export interface NormalizedEvent {
  kind: string;
  visibility: EventVisibility;
  payload: unknown;
  vendor_event_id: string | null;
  raw_segment_reference: string | null;
}

export interface EventEnvelope {
  event_id: string;
  session_id: string;
  run_id: string | null;
  sequence: number;
  timestamp: string;
  source: EventSource;
  event: NormalizedEvent;
}

export interface SessionRunStarted {
  sessionId: string;
  runId: string;
  processId: number;
}

export type SessionRunAttached = SessionRunStarted;

export interface SessionTerminalConnection {
  sessionId: string;
  terminal: TerminalOpened;
}

export type SessionTerminalStarted = SessionTerminalConnection;
export type SessionTerminalAttached = SessionTerminalConnection;

export type AgentKind = "codex" | "claude" | "agy" | "fake";
export type IntegrationMode = "structured" | "cli_managed" | "pty_tui";

export interface SessionIndexEntry {
  sessionId: string;
  projectId: string;
  agentKind: AgentKind;
  integrationMode: IntegrationMode;
  state: SessionState;
  title: string | null;
  activeRunId: string | null;
  latestSequence: number;
  updatedAt: string;
}

export interface SessionSnapshot {
  sessionId: string;
  activeRunId: string | null;
  state: SessionState;
  binding: string | null;
  latestSequence: number;
  droppedThroughSequence: number;
  stderr: string;
  stderrTruncated: boolean;
  lastExit: unknown;
  lastError: { code: string; message: string; correlationId: string } | null;
}

export interface SessionEventBatch {
  sessionId: string;
  events: EventEnvelope[];
  nextSequence: number;
  latestSequence: number;
  replayGap: {
    requestedAfterSequence: number;
    availableAfterSequence: number;
  } | null;
  state: SessionState;
}

export interface SessionRawBatch {
  sessionId: string;
  runId: string;
  data: number[];
  nextOffset: number;
  capturedBytes: number;
  observedBytes: number;
  truncated: boolean;
  complete: boolean;
}

export interface FakeSessionClient {
  attach(projectGrant: string, sessionId: string): Promise<SessionRunAttached>;
  attachTui(sessionId: string): Promise<SessionTerminalAttached>;
  startTui(
    projectGrant: string,
    scenario: string,
    columns: number,
    rows: number,
  ): Promise<SessionTerminalStarted>;
  listSessions(projectGrant: string, maximumSessions?: number): Promise<SessionIndexEntry[]>;
  start(
    projectGrant: string,
    scenario: string,
    binding?: string | null,
    volume?: number | null,
    captureRawProtocol?: boolean,
  ): Promise<SessionRunStarted>;
  resume(
    projectGrant: string,
    sessionId: string,
    scenario: string,
    binding?: string | null,
    captureRawProtocol?: boolean,
  ): Promise<SessionRunStarted>;
  snapshot(sessionId: string): Promise<SessionSnapshot>;
  readEvents(
    sessionId: string,
    afterSequence: number,
    signal?: AbortSignal,
  ): Promise<SessionEventBatch>;
  readRaw(
    sessionId: string,
    runId: string,
    afterOffset: number,
    maximumBytes?: number,
    signal?: AbortSignal,
  ): Promise<SessionRawBatch>;
  subscribe(sessionId: string, afterSequence: number): Promise<void>;
  unsubscribe(sessionId: string): Promise<void>;
  stop(sessionId: string): Promise<void>;
  respondPermission(
    sessionId: string,
    runId: string,
    requestId: string,
    decision: PermissionDecision,
  ): Promise<void>;
  respondUserInput(
    sessionId: string,
    runId: string,
    requestId: string,
    value: unknown,
  ): Promise<void>;
  sendGuiAction(
    sessionId: string,
    runId: string,
    action: string,
    payload: unknown,
  ): Promise<string>;
}

export type InvokeCommand = <T>(command: string, arguments_: Record<string, unknown>) => Promise<T>;

export function createTauriFakeSessionClient(
  invokeCommand: InvokeCommand = (command, arguments_) => invoke(command, arguments_),
): FakeSessionClient {
  return {
    attach(projectGrant, sessionId) {
      return invokeCommand<SessionRunAttached>("fake_session_attach", { projectGrant, sessionId });
    },
    attachTui(sessionId) {
      return invokeCommand<SessionTerminalAttached>("fake_tui_attach", { sessionId });
    },
    startTui(projectGrant, scenario, columns, rows) {
      return invokeCommand<SessionTerminalStarted>("fake_tui_start", {
        projectGrant,
        scenario,
        columns,
        rows,
      });
    },
    listSessions(projectGrant, maximumSessions = 50) {
      return invokeCommand<SessionIndexEntry[]>("session_list", {
        projectGrant,
        maximumSessions,
      });
    },
    start(projectGrant, scenario, binding = null, volume = null, captureRawProtocol = false) {
      return invokeCommand<SessionRunStarted>("fake_session_start", {
        projectGrant,
        scenario,
        binding,
        volume,
        captureRawProtocol,
      });
    },
    resume(projectGrant, sessionId, scenario, binding = null, captureRawProtocol = false) {
      return invokeCommand<SessionRunStarted>("fake_session_resume", {
        projectGrant,
        sessionId,
        scenario,
        binding,
        captureRawProtocol,
      });
    },
    snapshot(sessionId) {
      return invokeCommand<SessionSnapshot>("session_snapshot", { sessionId });
    },
    readEvents(sessionId, afterSequence, signal) {
      const read = invokeCommand<SessionEventBatch>("session_events_read", {
        sessionId,
        afterSequence,
      });
      return signal ? abortable(read, signal) : read;
    },
    readRaw(sessionId, runId, afterOffset, maximumBytes = 256 * 1024, signal) {
      const read = invokeCommand<SessionRawBatch>("session_raw_read", {
        sessionId,
        runId,
        afterOffset,
        maximumBytes,
      });
      return signal ? abortable(read, signal) : read;
    },
    subscribe(sessionId, afterSequence) {
      return invokeCommand<void>("session_subscribe", { sessionId, afterSequence });
    },
    unsubscribe(sessionId) {
      return invokeCommand<void>("session_unsubscribe", { sessionId });
    },
    stop(sessionId) {
      return invokeCommand<void>("session_stop", { sessionId });
    },
    respondPermission(sessionId, runId, requestId, decision) {
      return invokeCommand<void>("session_permission_respond", {
        sessionId,
        runId,
        requestId,
        decision,
      });
    },
    respondUserInput(sessionId, runId, requestId, value) {
      return invokeCommand<void>("session_user_input_respond", {
        sessionId,
        runId,
        requestId,
        valueJson: JSON.stringify(value),
      });
    },
    sendGuiAction(sessionId, runId, action, payload) {
      return invokeCommand<string>("session_gui_action", {
        sessionId,
        runId,
        action,
        payloadJson: JSON.stringify(payload),
      });
    },
  };
}

export const tauriFakeSessionClient = createTauriFakeSessionClient();

export const FAKE_SESSION_SCENARIOS = [
  ["structured/happy", "Happy structured stream"],
  ["structured/permission", "Permission request"],
  ["structured/user-input", "User-input request"],
  ["structured/gui-actions", "GUI action round trip"],
  ["structured/delay", "Delayed completion"],
  ["structured/nonzero", "Non-zero exit"],
  ["structured/crash", "Crash recovery"],
  ["structured/malformed", "Malformed protocol"],
  ["structured/incompatible", "Incompatible protocol"],
  ["structured/stall", "Stalled process (stop manually)"],
] as const;

export const FAKE_TUI_SCENARIOS = [
  ["tui/vt-baseline", "VT baseline and interactive input"],
  ["tui/alternate-screen", "Alternate-screen transition"],
  ["tui/resize-mouse", "Resize and mouse modes"],
  ["tui/osc-security", "OSC security filtering"],
] as const;

export const DEFAULT_UI_EVENT_LIMIT = 500;
export const DEFAULT_RAW_PROTOCOL_LIMIT = 1024 * 1024;

export function formatRawProtocolBytes(bytes: readonly number[]): string {
  let output = "";
  for (const byte of bytes) {
    if (byte === 0x0a) output += "\n";
    else if (byte === 0x0d) output += "\\r";
    else if (byte === 0x09) output += "\\t";
    else if (byte >= 0x20 && byte <= 0x7e) output += String.fromCharCode(byte);
    else output += `\\x${byte.toString(16).padStart(2, "0")}`;
  }
  return output;
}

export function mergeSessionEvents(
  current: readonly EventEnvelope[],
  incoming: readonly EventEnvelope[],
  maximumEvents = DEFAULT_UI_EVENT_LIMIT,
): EventEnvelope[] {
  if (maximumEvents <= 0) return [];
  const bySequence = new Map<number, EventEnvelope>();
  for (const event of current) bySequence.set(event.sequence, event);
  for (const event of incoming) bySequence.set(event.sequence, event);
  const ordered = [...bySequence.values()].sort((left, right) => left.sequence - right.sequence);
  return ordered.slice(-maximumEvents);
}

export interface RichEventProjection {
  id: string;
  sequence: number;
  source: EventSource;
  title: string;
  detail: string;
  tone: "default" | "info" | "success" | "warning" | "danger";
}

export function projectRichEvent(envelope: EventEnvelope): RichEventProjection {
  const { event } = envelope;
  const payload = objectPayload(event.payload);
  const safe = (key: string) => stringField(payload, key);
  let title = humanize(event.kind);
  let detail = `${envelope.source.toUpperCase()} normalized event`;
  let tone: RichEventProjection["tone"] = "default";

  switch (event.kind) {
    case "message_delta":
    case "message":
    case "delta":
      title = "Agent message";
      detail = safe("content") ?? "Message content was redacted or empty.";
      break;
    case "tool_start":
      title = `Tool started · ${safe("tool") ?? "unknown"}`;
      detail = safe("path") ?? "The fixture started a tool call.";
      tone = "info";
      break;
    case "tool_end":
      title = `Tool finished · ${safe("tool") ?? "unknown"}`;
      detail = safe("status") ?? "The fixture completed a tool call.";
      tone = safe("status") === "ok" ? "success" : "warning";
      break;
    case "permission_request":
      title = "Permission required";
      detail = commandSummary(payload);
      tone = "warning";
      break;
    case "user_input_request":
      title = "Input required";
      detail = safe("prompt") ?? "The session is waiting for input.";
      tone = "warning";
      break;
    case "usage":
      title = "Fixture usage";
      detail = `${numberField(payload, "input_tokens") ?? 0} input · ${numberField(payload, "output_tokens") ?? 0} output tokens`;
      tone = "info";
      break;
    case "result":
      title = "Run result";
      detail = safe("status") ?? "The structured run completed.";
      tone = "success";
      break;
    case "run_failed":
    case "protocol_error":
      title = "Run failed";
      detail = safe("category") ?? "The deterministic run failed.";
      tone = "danger";
      break;
    case "gui_permission_response":
      title = "Permission response sent";
      detail = `GUI → CLI permission.${safe("decision") ?? "cancel"}(…)`;
      tone = "info";
      break;
    case "gui_user_input_response":
      title = "Input response sent";
      detail = "GUI → CLI user_input.respond(value=[REDACTED])";
      tone = "info";
      break;
    case "gui_action":
      title = "GUI action sent";
      detail = `GUI → CLI ${safe("action") ?? "action"}(…)`;
      tone = "info";
      break;
    case "gui_stop_requested":
      title = "Stop requested";
      detail = "GUI → CLI session.stop(…)";
      tone = "warning";
      break;
    case "user_input_result":
    case "action_ack":
      detail = "Sensitive response content is not displayed.";
      tone = "success";
      break;
  }

  return {
    id: envelope.event_id,
    sequence: envelope.sequence,
    source: envelope.source,
    title,
    detail,
    tone,
  };
}

export function projectConsoleLine(envelope: EventEnvelope): string {
  const payload = objectPayload(envelope.event.payload);
  const requestId = stringField(payload, "request_id") ?? "unknown";
  const prefix = String(envelope.sequence).padStart(4, "0");
  switch (envelope.event.kind) {
    case "gui_permission_response":
      return `${prefix} GUI → CLI permission.${stringField(payload, "decision") ?? "cancel"}(request_id=${requestId})`;
    case "gui_user_input_response":
      return `${prefix} GUI → CLI user_input.respond(request_id=${requestId}, value=[REDACTED])`;
    case "gui_action":
      return `${prefix} GUI → CLI ${stringField(payload, "action") ?? "action"}(action_id=${stringField(payload, "action_id") ?? "unknown"})`;
    case "gui_stop_requested":
      return `${prefix} GUI → CLI session.stop(run_id=${stringField(payload, "run_id") ?? envelope.run_id ?? "unknown"})`;
    default: {
      const source = envelope.source === "cli" ? "CLI → GUI" : envelope.source.toUpperCase();
      return `${prefix} ${source} ${envelope.event.kind}${safeConsoleSummary(envelope)}`;
    }
  }
}

export function projectRawEvent(envelope: EventEnvelope): string {
  const safeEnvelope = {
    projection: "redacted_normalized_event",
    event_id: envelope.event_id,
    session_id: envelope.session_id,
    run_id: envelope.run_id,
    sequence: envelope.sequence,
    timestamp: envelope.timestamp,
    source: envelope.source,
    event: {
      kind: envelope.event.kind,
      visibility: envelope.event.visibility,
      payload: envelope.event.visibility === "sensitive"
        ? { redacted: true }
        : redactSuspiciousFields(envelope.event.payload),
      vendor_event_id: envelope.event.vendor_event_id,
      raw_segment_reference: envelope.event.raw_segment_reference,
    },
  };
  return JSON.stringify(safeEnvelope, null, 2);
}

export function isSessionSettled(state: SessionState): boolean {
  return ["completed", "stopped", "failed", "interrupted", "incompatible"].includes(state);
}

function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(abortError());
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(abortError());
    signal.addEventListener("abort", abort, { once: true });
    void promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

function abortError(): Error {
  const error = new Error("Session event read was cancelled.");
  error.name = "AbortError";
  return error;
}

function objectPayload(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function stringField(payload: Record<string, unknown>, key: string): string | null {
  const value = payload[key];
  return typeof value === "string" ? value : null;
}

function numberField(payload: Record<string, unknown>, key: string): number | null {
  const value = payload[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function commandSummary(payload: Record<string, unknown>): string {
  const command = payload.command;
  if (!Array.isArray(command)) return "The fixture requested a one-time permission decision.";
  const parts = command.filter((part): part is string => typeof part === "string");
  return parts.length > 0 ? parts.join(" ") : "The fixture requested a one-time permission decision.";
}

function safeConsoleSummary(envelope: EventEnvelope): string {
  if (envelope.event.visibility === "sensitive") return " · [REDACTED]";
  const payload = objectPayload(envelope.event.payload);
  const content = stringField(payload, "content");
  if (content) return ` · ${content.slice(0, 180)}`;
  const status = stringField(payload, "status");
  if (status) return ` · ${status}`;
  const tool = stringField(payload, "tool");
  if (tool) return ` · ${tool}`;
  return "";
}

function humanize(value: string): string {
  const words = value.replaceAll("_", " ");
  return words.length === 0 ? "Structured event" : words[0].toUpperCase() + words.slice(1);
}

function redactSuspiciousFields(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(redactSuspiciousFields);
  if (typeof value !== "object" || value === null) return value;
  return Object.fromEntries(Object.entries(value).map(([key, child]) => [
    key,
    /authorization|cookie|password|secret|token|api[_-]?key|value/i.test(key)
      ? "[REDACTED]"
      : redactSuspiciousFields(child),
  ]));
}
