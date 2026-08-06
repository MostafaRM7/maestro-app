import { useRef, useState, type FormEvent } from "react";
import type {
  FakeSessionController,
  PendingSessionRequest,
} from "../hooks/useFakeSession";
import {
  FAKE_SESSION_SCENARIOS,
  isSessionSettled,
  projectRichEvent,
  type EventEnvelope,
  type PermissionDecision,
  type SessionSnapshot,
  type SessionState,
} from "../lib/fakeSession";
import { Icon } from "./Icon";
import { WindowedList } from "./WindowedList";

interface FakeSessionWorkspaceProps {
  onOpenCompatibilityTui: () => void;
  projectName: string;
  session: FakeSessionController;
}

export function FakeSessionWorkspace({
  onOpenCompatibilityTui,
  projectName,
  session,
}: FakeSessionWorkspaceProps) {
  const [scenario, setScenario] = useState<string>(FAKE_SESSION_SCENARIOS[0][0]);
  const state = session.state;
  const hasOutcome = state !== null && (isSessionSettled(state) || state === "recoverable");
  const active = state !== null && !hasOutcome;

  if (!session.run) {
    return (
      <div className="fake-session-launcher">
        <h2 className="visually-hidden">{projectName}</h2>
        <div className="fake-session-launcher__mark" aria-hidden="true"><Icon name="spark" /></div>
        <p className="eyebrow">Milestone 0 integration harness</p>
        <h1>Run a deterministic fake structured session</h1>
        <p>
          This local fixture exercises Maestro's session UI and daemon transport. It is not
          Codex, Claude Code, or agy, and it never contacts an AI provider.
        </p>
        <label>
          Fixture scenario
          <select
            disabled={session.launching}
            onChange={(event) => setScenario(event.target.value)}
            value={scenario}
          >
            {FAKE_SESSION_SCENARIOS.map(([value, label]) => (
              <option key={value} value={value}>{label}</option>
            ))}
          </select>
        </label>
        <label className="raw-capture-option">
          <input
            checked={session.rawCaptureEnabled}
            disabled={session.launching}
            onChange={(event) => session.setRawCaptureEnabled(event.target.checked)}
            type="checkbox"
          />
          <span>
            Capture exact raw protocol bytes for this run
            <small>Off by default. Raw stdout is encrypted at rest but may contain unredacted secrets.</small>
          </span>
        </label>
        <button
          className="button button--primary"
          disabled={session.launching}
          onClick={() => void session.start(scenario)}
          type="button"
        >
          {session.launching ? "Starting fake session…" : "Start Fake Session"}
        </button>
        {session.error ? <p className="inline-error" role="alert">{session.error}</p> : null}
      </div>
    );
  }

  return (
    <div className="fake-session-workspace">
      <h2 className="visually-hidden">{projectName}</h2>
      <header className="fake-session-header">
        <div>
          <p className="eyebrow">Fake structured session · local fixture only</p>
          <h1>Normalized session stream</h1>
          <p className="fake-session-identifiers">
            Session {shortId(session.run.sessionId)} · Run {shortId(session.run.runId)} · PID {session.run.processId}
          </p>
        </div>
        <div className="fake-session-header__actions">
          <span className={`status-chip ${active ? "status-chip--success" : ""}`}>{stateLabel(state)}</span>
          {!state || !hasOutcome ? (
            <button
              className="button fake-session-stop"
              disabled={!active || session.stopping}
              onClick={() => void session.stop()}
              type="button"
            >
              {session.stopping ? "Stopping…" : "Stop Fake Session"}
            </button>
          ) : null}
        </div>
      </header>

      {session.replayGap ? (
        <div className="fake-session-warning" role="alert">
          Earlier fixture events exceeded bounded replay storage. The visible sequence starts at the oldest available event.
        </div>
      ) : null}
      {session.error ? <p className="inline-error fake-session-error" role="alert">{session.error}</p> : null}
      {state && hasOutcome ? (
        <SessionOutcome
          launching={session.launching}
          onOpenCompatibilityTui={onOpenCompatibilityTui}
          onResume={() => session.resume()}
          snapshot={session.snapshot}
          state={state}
        />
      ) : null}

      <div className="fake-session-feed" aria-label="Normalized fake session events">
        {session.pendingPermission || session.pendingInput || state === "ready" ? (
          <div className="fake-session-feed__actions">
            {session.pendingPermission ? (
              <PermissionRequest
                disabled={session.resolvedRequestIds.has(session.pendingPermission.requestId)}
                onRespond={(decision) => session.respondPermission(session.pendingPermission!, decision)}
                request={session.pendingPermission}
              />
            ) : null}
            {session.pendingInput ? (
              <UserInputRequest
                disabled={session.resolvedRequestIds.has(session.pendingInput.requestId)}
                onRespond={(value) => session.respondUserInput(session.pendingInput!, value)}
                request={session.pendingInput}
              />
            ) : null}
            {state === "ready" ? (
              <section className="session-request-card session-request-card--action">
                <div>
                  <p className="eyebrow">GUI → CLI annotation</p>
                  <h2>Exercise a correlated GUI action</h2>
                  <p>The payload travels through the fake CLI process; the event stream records only its safe annotation.</p>
                </div>
                <button className="button" onClick={() => void session.sendDemoGuiAction()} type="button">
                  Send session.inspect(…)
                </button>
              </section>
            ) : null}
          </div>
        ) : null}
        {session.events.length === 0 ? (
          <div className="fake-session-feed__empty" role="status">Waiting for the first normalized event…</div>
        ) : (
          <WindowedList
            aria-label="Normalized fake session event history"
            aria-live="polite"
            as="ol"
            className="rich-event-list"
            estimatedRowHeight={96}
            followEnd
            itemKey={richEventKey}
            items={session.events}
            keyboardScrollable
            renderItem={renderRichEvent}
          />
        )}
      </div>
    </div>
  );
}

const richEventKey = (event: EventEnvelope) => event.event_id;

function renderRichEvent(event: EventEnvelope) {
  const item = projectRichEvent(event);
  return (
    <article className={`normalized-event normalized-event--${item.tone}`}>
      <span className="normalized-event__sequence">{item.sequence}</span>
      <div>
        <div className="normalized-event__heading">
          <strong>{item.title}</strong>
          <span>{item.source}</span>
        </div>
        <p>{item.detail}</p>
      </div>
    </article>
  );
}

function PermissionRequest({
  disabled,
  onRespond,
  request,
}: {
  disabled: boolean;
  onRespond: (decision: PermissionDecision) => Promise<boolean>;
  request: PendingSessionRequest;
}) {
  const [submitted, setSubmitted] = useState(false);
  const submittedRef = useRef(false);
  const command = stringArray(request.payload.command).join(" ") || "Unknown command";
  const paths = stringArray(request.payload.paths);
  const respond = async (decision: PermissionDecision) => {
    if (disabled || submittedRef.current) return;
    submittedRef.current = true;
    setSubmitted(true);
    const delivered = await onRespond(decision).catch(() => false);
    if (!delivered) {
      submittedRef.current = false;
      setSubmitted(false);
    }
  };
  const controlsDisabled = disabled || submitted;
  return (
    <section className="session-request-card session-request-card--permission" aria-label="Permission request">
      <div>
        <p className="eyebrow">One-time permission request</p>
        <h2>{command}</h2>
        <p>{paths.length > 0 ? `Paths: ${paths.join(", ")}` : "No paths were declared."}</p>
        <code>{request.requestId}</code>
      </div>
      <div className="session-request-card__actions">
        <button className="button button--primary" disabled={controlsDisabled} onClick={() => void respond("allow")} type="button">Allow once</button>
        <button className="button" disabled={controlsDisabled} onClick={() => void respond("deny")} type="button">Deny</button>
        <button className="button" disabled={controlsDisabled} onClick={() => void respond("cancel")} type="button">Cancel request</button>
      </div>
    </section>
  );
}

function UserInputRequest({
  disabled,
  onRespond,
  request,
}: {
  disabled: boolean;
  onRespond: (value: unknown) => Promise<boolean>;
  request: PendingSessionRequest;
}) {
  const [value, setValue] = useState("");
  const [submitted, setSubmitted] = useState(false);
  const submittedRef = useRef(false);
  const choices = stringArray(request.payload.choices);
  const prompt = typeof request.payload.prompt === "string"
    ? request.payload.prompt
    : "The fake session requested input.";

  const deliver = async (response: unknown) => {
    if (disabled || submittedRef.current) return;
    submittedRef.current = true;
    setSubmitted(true);
    const delivered = await onRespond(response).catch(() => false);
    if (delivered) {
      setValue("");
      return;
    }
    submittedRef.current = false;
    setSubmitted(false);
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (disabled || submittedRef.current || value.length === 0) return;
    void deliver(value);
  };
  const cancel = () => {
    if (disabled || submittedRef.current) return;
    void deliver(null);
  };
  const controlsDisabled = disabled || submitted;

  return (
    <form className="session-request-card session-request-card--input" aria-label="User input request" onSubmit={submit}>
      <div>
        <p className="eyebrow">One-time user input</p>
        <h2>{prompt}</h2>
        <p>The response is sent through a sensitive transport and is not copied into display logs.</p>
      </div>
      {choices.length > 0 ? (
        <div className="session-input-choices" aria-label="Suggested answers">
          {choices.map((choice) => (
            <button disabled={controlsDisabled} key={choice} onClick={() => setValue(choice)} type="button">{choice}</button>
          ))}
        </div>
      ) : null}
      <label>
        Response
        <input
          autoComplete="off"
          disabled={controlsDisabled}
          onChange={(event) => setValue(event.target.value)}
          type="text"
          value={value}
        />
      </label>
      <div className="session-request-card__actions">
        <button className="button button--primary" disabled={controlsDisabled || value.length === 0} type="submit">Send once</button>
        <button className="button" disabled={controlsDisabled} onClick={cancel} type="button">Cancel input</button>
      </div>
    </form>
  );
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((part): part is string => typeof part === "string") : [];
}

const STDERR_PREVIEW_LIMIT = 4_096;

function SessionOutcome({
  launching,
  onOpenCompatibilityTui,
  onResume,
  snapshot,
  state,
}: {
  launching: boolean;
  onOpenCompatibilityTui: () => void;
  onResume: () => Promise<void>;
  snapshot: SessionSnapshot | null;
  state: SessionState;
}) {
  const content = outcomeContent(state);
  const stderr = boundedText(snapshot?.stderr ?? "", STDERR_PREVIEW_LIMIT);
  const stderrWasBounded = (snapshot?.stderr.length ?? 0) > STDERR_PREVIEW_LIMIT;
  const error = snapshot?.lastError;
  const recoveryAction = recoveryActionFor(state);

  return (
    <section className="session-request-card session-request-card--action" aria-label="Session outcome">
      <div>
        <p className="eyebrow">Final session outcome</p>
        <h2>{content.title}</h2>
        <p>{content.detail}</p>
        {!snapshot ? <p role="status">Loading the final daemon snapshot…</p> : (
          <dl>
            <dt>Last exit</dt>
            <dd>{safeExitLabel(snapshot.lastExit)}</dd>
            {error ? (
              <>
                <dt>Error code</dt>
                <dd>{boundedText(error.code, 128)}</dd>
                <dt>Error message</dt>
                <dd>{boundedText(error.message, 2_048)}</dd>
                <dt>Correlation ID</dt>
                <dd><code>{boundedText(error.correlationId, 128)}</code></dd>
              </>
            ) : null}
          </dl>
        )}
        {stderr ? (
          <div>
            <strong>Bounded stderr</strong>
            <pre aria-label="Bounded session stderr">{stderr}</pre>
            {snapshot?.stderrTruncated || stderrWasBounded ? (
              <p role="status">
                {snapshot?.stderrTruncated
                  ? "Earlier stderr was truncated by the daemon retention limit."
                  : "This stderr preview was truncated by the UI display limit."}
              </p>
            ) : null}
          </div>
        ) : null}
      </div>
      {recoveryAction === "compatibility" ? (
        <button className="button" onClick={onOpenCompatibilityTui} type="button">
          Open Exact TUI Compatibility Mode
        </button>
      ) : recoveryAction ? (
        <button className="button" disabled={launching} onClick={() => void onResume()} type="button">
          {launching ? "Starting recovery…" : recoveryAction}
        </button>
      ) : null}
    </section>
  );
}

type RecoveryAction =
  | "compatibility"
  | "Attempt Supported Recovery"
  | "Retry in a New Fixture Run"
  | "Start a New Fixture Run"
  | "Start a Follow-up Fixture Run";

function recoveryActionFor(state: SessionState): RecoveryAction | null {
  switch (state) {
    case "incompatible":
      return "compatibility";
    case "interrupted":
    case "recoverable":
      return "Attempt Supported Recovery";
    case "failed":
      return "Retry in a New Fixture Run";
    case "stopped":
      return "Start a New Fixture Run";
    case "completed":
      return "Start a Follow-up Fixture Run";
    default:
      return null;
  }
}

function outcomeContent(state: SessionState): { detail: string; title: string } {
  switch (state) {
    case "completed":
      return {
        detail: "The CLI reported successful completion. A follow-up starts a separate run.",
        title: "Session completed successfully",
      };
    case "stopped":
      return {
        detail: "The process was stopped explicitly. Starting again creates a separate run.",
        title: "Session stopped by request",
      };
    case "failed":
      return {
        detail: "The CLI process or structured protocol failed. Review the bounded diagnostics before retrying.",
        title: "Session failed",
      };
    case "interrupted":
      return {
        detail: "The previous process was interrupted. Exact continuation is unavailable; Maestro can only attempt the vendor-supported recovery path.",
        title: "Session was interrupted",
      };
    case "recoverable":
      return {
        detail: "The CLI reported that a supported recovery attempt is available. It may start a new execution point.",
        title: "Session is recoverable",
      };
    case "incompatible":
      return {
        detail: "This structured protocol is incompatible with Maestro. Use the original CLI through exact TUI compatibility mode.",
        title: "Structured integration is incompatible",
      };
    default:
      return { detail: "The session reached a terminal state.", title: "Session ended" };
  }
}

function safeExitLabel(value: unknown): string {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return "Not reported";
  const exit = value as Record<string, unknown>;
  if (exit.cause === "unknown") return "Unknown exit cause";
  if (!Number.isSafeInteger(exit.value)) return "Invalid exit data was withheld";
  if (exit.cause === "exited") return `Exited with code ${String(exit.value)}`;
  if (exit.cause === "signaled") return `Terminated by signal ${String(exit.value)}`;
  return "Invalid exit data was withheld";
}

function boundedText(value: string, maximumCharacters: number): string {
  return value.slice(0, maximumCharacters);
}

function stateLabel(state: SessionState | null): string {
  if (!state) return "Connecting";
  return state.split("_").map((part) => part[0]?.toUpperCase() + part.slice(1)).join(" ");
}

function shortId(value: string): string {
  return value.slice(0, 8);
}
