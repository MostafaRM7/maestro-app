import { lazy, Suspense, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import type { BottomSurface } from "../hooks/useWindowLayout";
import { requestTerminalLinkOpen } from "../lib/terminalLinkClient";
import type { FakeSessionController } from "../hooks/useFakeSession";
import type { FakeTuiController } from "../hooks/useFakeTui";
import type { ShellTerminalController } from "../hooks/useShellTerminal";
import { FAKE_TUI_SCENARIOS, formatRawProtocolBytes, isSessionSettled, projectConsoleLine, type SessionRawBatch } from "../lib/fakeSession";
import type { DaemonSnapshot } from "../lib/system";
import { Icon } from "./Icon";
import { WindowedList } from "./WindowedList";

const TerminalSurface = lazy(async () => {
  const module = await import("./TerminalSurface");
  return { default: module.TerminalSurface };
});

interface BottomPanelProps {
  activeSurface: BottomSurface;
  daemon: DaemonSnapshot;
  fakeSession: FakeSessionController;
  fakeTui: FakeTuiController;
  onClose: () => void;
  onSelectSurface: (surface: BottomSurface) => void;
  open: boolean;
  shellTerminal: ShellTerminalController;
}

const surfaces: ReadonlyArray<{ id: BottomSurface; label: string }> = [
  { id: "events", label: "Events" },
  { id: "raw", label: "Raw" },
  { id: "agent", label: "Agent TUI" },
  { id: "shell", label: "Shell" },
];

export function BottomPanel({ activeSurface, daemon, fakeSession, fakeTui, onClose, onSelectSurface, open, shellTerminal }: BottomPanelProps) {
  if (!open) return null;

  return (
    <section className="bottom-panel" data-focus-zone tabIndex={-1} aria-label="Console panel">
      <div className="bottom-panel__tabs" role="tablist" aria-label="Console views">
        {surfaces.map((surface) => (
          <button
            aria-selected={activeSurface === surface.id}
            aria-controls={`bottom-panel-${surface.id}`}
            className={activeSurface === surface.id ? "is-active" : ""}
            key={surface.id}
            onClick={() => onSelectSurface(surface.id)}
            onKeyDown={(event) => handleTabKey(event, surface.id, onSelectSurface)}
            role="tab"
            id={`bottom-tab-${surface.id}`}
            tabIndex={activeSurface === surface.id ? 0 : -1}
            type="button"
          >
            {surface.label}{surface.id === "events" ? <span className="count-badge">{fakeSession.events.length}</span> : null}
          </button>
        ))}
        <div className="bottom-panel__spacer" />
        <button className="icon-button icon-button--small" aria-label="Close bottom panel" onClick={onClose} type="button"><Icon name="x" /></button>
      </div>
      <div className="bottom-panel__content">
        {surfaceIds.filter((surface) => surface !== "shell").map((surface) => (
          <div aria-labelledby={`bottom-tab-${surface}`} hidden={activeSurface !== surface} id={`bottom-panel-${surface}`} key={surface} role="tabpanel">
            <BottomSurfaceContent
              active={activeSurface === surface}
              daemon={daemon}
              session={fakeSession}
              surface={surface}
              tui={fakeTui}
            />
          </div>
        ))}
        <div aria-labelledby="bottom-tab-shell" hidden={activeSurface !== "shell"} id="bottom-panel-shell" role="tabpanel">
          <ShellSurface active={activeSurface === "shell"} daemon={daemon} shell={shellTerminal} />
        </div>
      </div>
    </section>
  );
}

function ShellSurface({ active, daemon, shell }: { active: boolean; daemon: DaemonSnapshot; shell: ShellTerminalController }) {
  if ((shell.phase === "running" || shell.phase === "stopping") && shell.transport) {
    return (
      <div className="shell-terminal">
        <div className="shell-terminal__toolbar">
          <span className="shell-terminal__status"><span className="status-led status-led--success" />{shell.phase === "stopping" ? "Stopping shell…" : "Shell running"}</span>
          <span className="shell-terminal__cwd" title={shell.canonicalCwd ?? undefined}>{shell.canonicalCwd}</span>
          <button className="button button--terminal-danger" disabled={shell.phase === "stopping"} onClick={() => void shell.stop()} type="button">Stop Terminal</button>
        </div>
        {shell.overflowed || shell.replayTruncated ? (
          <div className="terminal-warning" role="alert">
            Some earlier terminal output is unavailable{shell.droppedThroughSequence === null ? "." : ` (daemon sequence ${shell.droppedThroughSequence} and earlier was dropped).`}
          </div>
        ) : null}
        <div className="shell-terminal__viewport">
          <Suspense fallback={<div className="terminal-loading" role="status">Preparing terminal renderer…</div>}>
            <TerminalSurface active={active} ariaLabel="Shell terminal" onLinkRequest={requestTerminalLinkOpen} transport={shell.transport} />
          </Suspense>
        </div>
      </div>
    );
  }

  if (shell.phase === "starting") {
    return <ShellStatus icon="terminal" title="Starting shell terminal…" detail="Maestro is asking the daemon to open a PTY in the project folder." progress />;
  }

  if (shell.phase === "attaching") {
    return <ShellStatus icon="terminal" title="Reattaching shell terminal…" detail="Maestro is reconnecting this view to the existing daemon-owned PTY without launching another process." progress />;
  }

  const daemonAvailable = daemon.status === "connected";
  if (!daemonAvailable && shell.phase === "idle") {
    return (
      <ShellStatus
        action="Start Shell Terminal"
        actionDisabled
        detail="Reconnect the Maestro service before starting a daemon-owned PTY session."
        icon="info"
        title="Shell terminals are unavailable while the daemon is offline."
      />
    );
  }

  if (shell.phase === "idle") {
    return (
      <div className="shell-outcome">
        <ShellStatus action="Start Shell Terminal" onAction={() => void shell.start()} detail="The shell will run in the selected project's canonical folder and remain daemon-owned if this panel is hidden." icon="terminal" title="No shell terminal is attached to this view." />
        <ShellTerminalIndex shell={shell} />
      </div>
    );
  }

  const title = terminalOutcomeTitle(shell);
  const detail = shell.error ?? terminalOutcomeDetail(shell);
  return (
    <div className="shell-outcome">
      {shell.overflowed || shell.replayTruncated ? (
        <div className="terminal-warning" role="alert">
          Some earlier terminal output was unavailable before this terminal ended{shell.droppedThroughSequence === null ? "." : ` (daemon sequence ${shell.droppedThroughSequence} and earlier was dropped).`}
        </div>
      ) : null}
      <ShellStatus action="Start New Terminal" actionDisabled={!daemonAvailable} onAction={() => void shell.start()} detail={detail} icon="info" title={title} />
    </div>
  );
}

function ShellTerminalIndex({ shell }: { shell: ShellTerminalController }) {
  return (
    <section className="persisted-session-index" aria-label="Daemon-owned shell terminals">
      <header>
        <div><strong>Project shell terminals</strong><span>Reconnect without restarting the shell</span></div>
        <button className="text-button" disabled={shell.listLoading} onClick={() => void shell.reloadTerminals()} type="button">
          {shell.listLoading ? "Loading…" : "Refresh"}
        </button>
      </header>
      {shell.listError ? <span className="terminal-inline-error" role="alert">{shell.listError}</span> : null}
      {!shell.listLoading && shell.terminals.length === 0 ? <p>No retained shell terminals for this project.</p> : null}
      {shell.terminals.length > 0 ? (
        <ol>
          {shell.terminals.map((terminal) => (
            <li key={terminal.terminalId}>
              <span><strong>{terminal.title}</strong><small>{terminal.state} · PID {terminal.processId} · {terminal.canonicalCwd}</small></span>
              <button
                aria-label={`Attach shell ${terminal.processId}`}
                className="text-button"
                disabled={terminal.state !== "running" || shell.phase === "attaching"}
                onClick={() => void shell.attach(terminal.terminalId)}
                type="button"
              >
                Attach
              </button>
            </li>
          ))}
        </ol>
      ) : null}
    </section>
  );
}

function terminalOutcomeTitle(shell: ShellTerminalController) {
  if (shell.phase === "disconnected") return "The terminal connection was lost.";
  if (shell.phase === "failed") return "The shell terminal could not continue.";
  if (shell.phase === "closed") return "The shell terminal was stopped.";
  return "The shell process exited.";
}

function terminalOutcomeDetail(shell: ShellTerminalController) {
  if (shell.phase === "exited" && shell.exitCode !== null) return `The process exited with code ${shell.exitCode}.`;
  if (shell.phase === "closed") return "Start a new daemon-owned PTY whenever you are ready.";
  return "Start a new terminal to continue working in this project.";
}

function ShellStatus({
  action,
  actionDisabled = false,
  detail,
  icon,
  onAction,
  progress = false,
  title,
}: {
  action?: string;
  actionDisabled?: boolean;
  detail: string;
  icon: "info" | "terminal";
  onAction?: () => void;
  progress?: boolean;
  title: string;
}) {
  return (
    <div className="console-empty shell-status">
      <Icon name={icon} />
      <div><strong>{title}</strong><span>{detail}</span>{progress ? <span className="shell-status__progress" role="status">Opening terminal process</span> : null}</div>
      {action ? <button className="button button--terminal" disabled={actionDisabled} onClick={onAction} type="button">{action}</button> : null}
    </div>
  );
}

const surfaceIds: readonly BottomSurface[] = ["events", "raw", "agent", "shell"];

function handleTabKey(event: ReactKeyboardEvent<HTMLButtonElement>, current: BottomSurface, select: (surface: BottomSurface) => void) {
  const currentIndex = surfaceIds.indexOf(current);
  let nextIndex: number | null = null;
  if (event.key === "ArrowRight") nextIndex = (currentIndex + 1) % surfaceIds.length;
  if (event.key === "ArrowLeft") nextIndex = (currentIndex - 1 + surfaceIds.length) % surfaceIds.length;
  if (event.key === "Home") nextIndex = 0;
  if (event.key === "End") nextIndex = surfaceIds.length - 1;
  if (nextIndex === null) return;
  event.preventDefault();
  const next = surfaceIds[nextIndex];
  select(next);
  document.getElementById(`bottom-tab-${next}`)?.focus();
}

function BottomSurfaceContent({
  active,
  daemon,
  surface,
  session,
  tui,
}: {
  active: boolean;
  daemon: DaemonSnapshot;
  surface: BottomSurface;
  session: FakeSessionController;
  tui: FakeTuiController;
}) {
  if (surface === "events") {
    if (session.events.length === 0) {
      return <ConsoleEmpty icon="spark" title="Session events will appear here." detail="GUI actions and CLI lifecycle events share one readable ledger." />;
    }
    return (
      <WindowedList
        aria-label="Human-readable fake session event console"
        as="ol"
        className="event-console"
        estimatedRowHeight={32}
        followEnd
        itemKey={eventKey}
        items={session.events}
        keyboardScrollable
        renderItem={renderEvent}
      />
    );
  }
  if (surface === "raw") {
    if (!active) return null;
    if (!session.run) {
      return <ConsoleEmpty icon="info" title="Raw capture is off by default." detail="Enable sensitive raw capture before launching a structured session to inspect exact CLI stdout bytes." />;
    }
    if (!session.rawCaptureEnabled) {
      return <ConsoleEmpty icon="warning" title="Raw capture was not enabled for this run." detail="Normalized events stay available in the Events tab. Start another run with explicit raw capture if exact protocol bytes are required." />;
    }
    if (session.rawCaptureError) {
      return <ConsoleEmpty icon="warning" title="Sensitive raw capture is unavailable." detail={session.rawCaptureError} />;
    }
    if (!session.rawProtocol) {
      return <ConsoleEmpty icon="info" title="Waiting for raw protocol bytes…" detail="Capture is enabled for this run and remains bounded and encrypted at rest." />;
    }
    return <RawProtocolInspector capture={session.rawProtocol} key={`${session.rawProtocol.sessionId}:${session.rawProtocol.runId}`} />;
  }
  if (surface === "agent") {
    return <AgentTuiSurface active={active} daemon={daemon} session={session} tui={tui} />;
  }
  return <ConsoleEmpty icon="terminal" title="No terminal tabs." detail="Shell terminals are independent daemon-owned PTY sessions." />;
}

const RAW_PROTOCOL_PAGE_BYTES = 16 * 1024;

function RawProtocolInspector({ capture }: { capture: SessionRawBatch }) {
  const [page, setPage] = useState(0);
  const pageCount = Math.max(1, Math.ceil(capture.data.length / RAW_PROTOCOL_PAGE_BYTES));
  const safePage = Math.min(page, pageCount - 1);
  const start = safePage * RAW_PROTOCOL_PAGE_BYTES;
  const end = Math.min(capture.data.length, start + RAW_PROTOCOL_PAGE_BYTES);
  const visibleBytes = capture.data.slice(start, end);

  return (
    <div className="raw-protocol-inspector">
      <div className="raw-protocol-inspector__warning" role="alert">
        SENSITIVE — exact, unredacted CLI stdout. This data may contain credentials, file contents, or private prompts.
      </div>
      <div className="raw-protocol-inspector__metadata">
        <span className="raw-protocol-inspector__summary" role="status">
          <span>
            Captured {capture.capturedBytes.toLocaleString()} of {capture.observedBytes.toLocaleString()} observed bytes
            {capture.truncated ? " · truncated at the hard capture limit" : ""}
            {capture.complete ? " · complete" : " · live"}
          </span>
          <span>
            Displaying retained bytes {capture.data.length === 0 ? "0" : `${(start + 1).toLocaleString()}–${end.toLocaleString()}`} of {capture.data.length.toLocaleString()} in bounded 16 KiB pages
          </span>
        </span>
        <span className="raw-protocol-inspector__paging">
          <button className="text-button" disabled={safePage === 0} onClick={() => setPage((current) => Math.max(0, current - 1))} type="button">Previous bytes</button>
          <span>Page {safePage + 1} of {pageCount}</span>
          <button className="text-button" disabled={safePage >= pageCount - 1} onClick={() => setPage((current) => Math.min(pageCount - 1, current + 1))} type="button">Next bytes</button>
        </span>
      </div>
      <pre aria-label="Sensitive exact raw protocol bytes">
        {formatRawProtocolBytes(visibleBytes)}
      </pre>
    </div>
  );
}

const eventKey = (event: FakeSessionController["events"][number]) => event.event_id;
const renderEvent = (event: FakeSessionController["events"][number]) => projectConsoleLine(event);

function AgentTuiSurface({ active, daemon, session, tui }: { active: boolean; daemon: DaemonSnapshot; session: FakeSessionController; tui: FakeTuiController }) {
  const [scenario, setScenario] = useState<string>(FAKE_TUI_SCENARIOS[0][0]);
  const canStart = daemon.status === "connected" && tui.phase !== "attaching" && tui.phase !== "starting" && tui.phase !== "stopping";

  if (tui.transport) {
    const canStop = tui.sessionId !== null && tui.phase !== "exited";
    return (
      <div className="shell-terminal fake-agent-tui">
        <div className="shell-terminal__toolbar">
          <span className="shell-terminal__status">
            <span className={`status-led ${tui.phase === "running" ? "status-led--success" : ""}`} />
            Fake exact TUI · {tui.phase}
          </span>
          <span className="shell-terminal__cwd">Local deterministic fixture — no AI provider</span>
          {canStop ? (
            <button
              className="button button--terminal-danger"
              disabled={tui.phase === "stopping"}
              onClick={() => void tui.stop()}
              type="button"
            >
              {tui.phase === "stopping" ? "Stopping fake TUI…" : "Stop Fake TUI"}
            </button>
          ) : (
            <button className="button button--terminal" disabled={!canStart} onClick={() => void tui.start(scenario)} type="button">
              Start New Fake TUI
            </button>
          )}
        </div>
        {tui.snapshot?.overflowed || tui.snapshot?.replayTruncated ? (
          <div className="terminal-warning" role="alert">
            Some earlier fake TUI output is unavailable because bounded terminal retention was exceeded.
          </div>
        ) : null}
        {tui.error ? <div className="terminal-warning" role="alert">{tui.error}</div> : null}
        <div className="shell-terminal__viewport">
          <Suspense fallback={<div className="terminal-loading" role="status">Preparing exact fake TUI renderer…</div>}>
            <TerminalSurface active={active} ariaLabel="Exact fake agent TUI" onLinkRequest={requestTerminalLinkOpen} transport={tui.transport} />
          </Suspense>
        </div>
      </div>
    );
  }

  if (tui.phase === "starting") {
    return <ShellStatus icon="terminal" title="Starting exact fake TUI…" detail="Maestro is launching its deterministic fake-agent executable in a daemon-owned PTY." progress />;
  }

  if (tui.phase === "attaching") {
    return <ShellStatus icon="terminal" title="Reattaching exact fake TUI…" detail="Maestro is reconnecting this view to the existing daemon-owned PTY without launching another process." progress />;
  }

  return (
    <div className="fake-tui-empty">
      <div className="fake-tui-launcher">
        <div>
          <p className="eyebrow">Milestone 0 exact-TUI harness</p>
          <strong>This is Maestro's local fake agent, not Codex, Claude Code, or agy.</strong>
          <span>The original alternate screen, cursor control, ANSI output, resize, mouse input, and interactive stdin run through one daemon-owned PTY.</span>
        </div>
        <label>
          Fake TUI scenario
          <select disabled={!canStart} onChange={(event) => setScenario(event.target.value)} value={scenario}>
            {FAKE_TUI_SCENARIOS.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
          </select>
        </label>
        <button className="button button--terminal" disabled={!canStart} onClick={() => void tui.start(scenario)} type="button">
          Start Exact Fake TUI
        </button>
        {daemon.status !== "connected" ? <span role="status">Reconnect the Maestro service to launch the fake PTY.</span> : null}
        {tui.error ? <span className="terminal-inline-error" role="alert">{tui.error}</span> : null}
      </div>
      <PersistedSessionIndex session={session} tui={tui} />
    </div>
  );
}

function PersistedSessionIndex({ session, tui }: { session: FakeSessionController; tui: FakeTuiController }) {
  const sessions = session.sessions.length > 0 ? session.sessions : tui.sessions;
  const loading = session.listLoading || tui.listLoading;
  const listError = session.listError ?? tui.listError;
  return (
    <section className="persisted-session-index" aria-label="Persisted project sessions">
      <header>
        <div>
          <strong>Persisted project sessions</strong>
          <span>Reconnect to active exact-TUI sessions</span>
        </div>
        <button className="text-button" disabled={loading} onClick={() => void Promise.all([session.reloadSessions(), tui.reloadSessions()])} type="button">
          {loading ? "Loading…" : "Refresh"}
        </button>
      </header>
      {listError ? <span className="terminal-inline-error" role="alert">{listError}</span> : null}
      {!loading && sessions.length === 0 ? <p>No persisted sessions for this project.</p> : null}
      {sessions.length > 0 ? (
        <ol>
          {sessions.map((entry) => {
            const tuiAttachable = entry.integrationMode === "pty_tui" && entry.activeRunId !== null && entry.state === "running";
            const structuredAttachable = entry.integrationMode === "structured"
              && entry.activeRunId !== null
              && !isSessionSettled(entry.state);
            const isCurrentTui = tui.sessionId === entry.sessionId && tui.transport !== null;
            const isCurrentStructured = session.run?.sessionId === entry.sessionId;
            return (
              <li key={entry.sessionId}>
                <span><strong>{entry.title ?? "Untitled fake session"}</strong><small>{entry.integrationMode.replaceAll("_", " ")} · {entry.state}</small></span>
                <span className="persisted-session-index__actions">
                  <span className="support-badge">{entry.agentKind}</span>
                  {tuiAttachable ? (
                    <button
                      aria-label={`Attach ${entry.title ?? "untitled fake TUI"}`}
                      className="text-button"
                      disabled={isCurrentTui || tui.transport !== null || tui.phase === "attaching" || tui.phase === "starting" || tui.phase === "stopping"}
                      onClick={() => void tui.attach(entry.sessionId)}
                      type="button"
                    >
                      {isCurrentTui ? "Attached" : "Attach TUI"}
                    </button>
                  ) : null}
                  {structuredAttachable ? (
                    <button
                      aria-label={`Attach ${entry.title ?? "untitled structured session"}`}
                      className="text-button"
                      disabled={isCurrentStructured || session.launching}
                      onClick={() => void session.attach(entry.sessionId)}
                      type="button"
                    >
                      {isCurrentStructured ? "Attached" : "Attach structured"}
                    </button>
                  ) : null}
                </span>
              </li>
            );
          })}
        </ol>
      ) : null}
      <p>Active structured and exact-TUI sessions reconnect to their existing daemon-owned processes without launching or resuming.</p>
    </section>
  );
}

function ConsoleEmpty({ action, detail, icon, title }: { action?: string; detail: string; icon: "info" | "spark" | "terminal" | "warning"; title: string }) {
  return (
    <div className="console-empty">
      <Icon name={icon} />
      <div><strong>{title}</strong><span>{detail}</span></div>
      {action ? <button aria-describedby="terminal-unavailable" className="text-button" disabled type="button">+ {action}</button> : null}
      {action ? <span className="visually-hidden" id="terminal-unavailable">A live daemon terminal transport is not connected.</span> : null}
    </div>
  );
}
