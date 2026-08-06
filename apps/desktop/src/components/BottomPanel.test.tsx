import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { FakeSessionController } from "../hooks/useFakeSession";
import type { FakeTuiController } from "../hooks/useFakeTui";
import type { ShellTerminalController } from "../hooks/useShellTerminal";
import type { EventEnvelope } from "../lib/fakeSession";
import { BottomPanel } from "./BottomPanel";

vi.mock("./TerminalSurface", () => ({
  TerminalSurface: ({ ariaLabel }: { ariaLabel: string }) => <div aria-label={ariaLabel} role="region" />,
}));

function structured(overrides: Partial<FakeSessionController> = {}): FakeSessionController {
  return {
    attach: vi.fn().mockResolvedValue(undefined),
    error: null,
    events: [],
    launching: false,
    listError: null,
    listLoading: false,
    pendingInput: null,
    pendingPermission: null,
    replayGap: false,
    rawCaptureEnabled: false,
    rawCaptureError: null,
    rawProtocol: null,
    resolvedRequestIds: new Set(),
    reloadSessions: vi.fn().mockResolvedValue(undefined),
    run: null,
    snapshot: null,
    sessions: [],
    state: null,
    stopping: false,
    setRawCaptureEnabled: vi.fn(),
    respondPermission: vi.fn(),
    respondUserInput: vi.fn(),
    resume: vi.fn(),
    sendDemoGuiAction: vi.fn(),
    start: vi.fn(),
    stop: vi.fn(),
    ...overrides,
  };
}

function tui(): FakeTuiController {
  return {
    attach: vi.fn().mockResolvedValue(undefined),
    error: null,
    listError: null,
    listLoading: false,
    phase: "idle",
    sessions: [{
      activeRunId: null,
      agentKind: "fake",
      integrationMode: "structured",
      latestSequence: 8,
      projectId: "persisted-project",
      sessionId: "persisted-session",
      state: "completed",
      title: "Fake · structured/happy",
      updatedAt: "2026-08-05T10:00:00Z",
    }],
    sessionId: null,
    snapshot: null,
    transport: null,
    reloadSessions: vi.fn().mockResolvedValue(undefined),
    start: vi.fn().mockResolvedValue(undefined),
    stop: vi.fn().mockResolvedValue(undefined),
  };
}

function eventEnvelope(index: number): EventEnvelope {
  return {
    event_id: `event-${index}`,
    event: {
      kind: "assistant_message",
      payload: { text: `Event ${index}` },
      raw_segment_reference: null,
      vendor_event_id: null,
      visibility: "user",
    },
    run_id: "run-1",
    sequence: index,
    session_id: "session-1",
    source: "cli",
    timestamp: "2026-08-05T10:00:00Z",
  };
}

function panel(session: FakeSessionController, activeSurface: "events" | "raw" = "events") {
  return (
    <BottomPanel
      activeSurface={activeSurface}
      daemon={{
        detail: "Connected",
        status: "connected",
        storageSchemaVersion: 3,
        storageStatus: "ready",
      }}
      fakeSession={session}
      fakeTui={tui()}
      onClose={vi.fn()}
      onSelectSurface={vi.fn()}
      open
      shellTerminal={{} as ShellTerminalController}
    />
  );
}

describe("BottomPanel exact fake TUI", () => {
  it("keeps a bounded event DOM for large histories", () => {
    render(panel(structured({ events: Array.from({ length: 1_000 }, (_, index) => eventEnvelope(index)) })));

    const console = screen.getByRole("list", { name: "Human-readable fake session event console" });
    expect(within(console).getAllByRole("listitem").length).toBeLessThan(50);
    expect(console).toHaveAttribute("aria-label", "Human-readable fake session event console");
    expect(console).toHaveAttribute("tabindex", "0");
  });

  it("follows appended events only while the console is already at the end", async () => {
    const initialEvents = Array.from({ length: 20 }, (_, index) => eventEnvelope(index));
    const { rerender } = render(panel(structured({ events: initialEvents })));
    const console = screen.getByRole("list", { name: "Human-readable fake session event console" });
    Object.defineProperties(console, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 1_000 },
    });

    console.scrollTop = 900;
    fireEvent.scroll(console);
    rerender(panel(structured({ events: [...initialEvents, eventEnvelope(20)] })));
    await waitFor(() => expect(console.scrollTop).toBe(900));

    console.scrollTop = 200;
    fireEvent.scroll(console);
    rerender(panel(structured({ events: [...initialEvents, eventEnvelope(20), eventEnvelope(21)] })));
    await waitFor(() => expect(console.scrollTop).toBe(200));
  });

  it("keeps following the end after virtualized rows report taller measured heights", async () => {
    const measured = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const height = this.hasAttribute("data-windowed-key") ? 64 : 0;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      };
    });
    const initialEvents = Array.from({ length: 101 }, (_, index) => eventEnvelope(index));
    const view = render(panel(structured({ events: initialEvents })));
    const console = screen.getByRole("list", { name: "Human-readable fake session event console" });
    Object.defineProperties(console, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    await waitFor(() => expect(Number.parseFloat(console.style.getPropertyValue("--windowed-content-height"))).toBeGreaterThan(101 * 32));
    const before = Number.parseFloat(console.style.getPropertyValue("--windowed-content-height"));
    console.scrollTop = before - 100;
    fireEvent.scroll(console);

    view.rerender(panel(structured({ events: [...initialEvents, eventEnvelope(101)] })));

    await waitFor(() => {
      const after = Number.parseFloat(console.style.getPropertyValue("--windowed-content-height"));
      expect(after).toBeGreaterThan(before);
      expect(console.scrollTop).toBe(after - 100);
    });
    measured.mockRestore();
  });

  it("does not project sensitive raw bytes while Raw is inactive", () => {
    const rawProtocol = {
      capturedBytes: 8,
      complete: true,
      data: [] as number[],
      nextOffset: 8,
      observedBytes: 8,
      runId: "run-1",
      sessionId: "session-1",
      truncated: false,
    };
    const readData = vi.fn(() => [115, 101, 99, 114, 101, 116]);
    Object.defineProperty(rawProtocol, "data", { configurable: true, get: readData });

    render(panel(structured({
      rawCaptureEnabled: true,
      rawProtocol,
      run: { processId: 42, runId: "run-1", sessionId: "session-1" },
    })));

    expect(readData).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Sensitive exact raw protocol bytes")).not.toBeInTheDocument();
    expect(screen.queryByText(/SENSITIVE — exact/)).not.toBeInTheDocument();
  });

  it("shows exact opted-in bytes only behind a prominent sensitive warning", () => {
    render(
      <BottomPanel
        activeSurface="raw"
        daemon={{
          detail: "Connected",
          status: "connected",
          storageSchemaVersion: 3,
          storageStatus: "ready",
        }}
        fakeSession={structured({
          rawCaptureEnabled: true,
          rawProtocol: {
            capturedBytes: 22,
            complete: true,
            data: [...new TextEncoder().encode("{\"token\":\"secret\"}\n")],
            nextOffset: 22,
            observedBytes: 30,
            runId: "run-1",
            sessionId: "session-1",
            truncated: true,
          },
          run: { processId: 42, runId: "run-1", sessionId: "session-1" },
        })}
        fakeTui={tui()}
        onClose={vi.fn()}
        onSelectSurface={vi.fn()}
        open
        shellTerminal={{} as ShellTerminalController}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("SENSITIVE");
    expect(screen.getByLabelText("Sensitive exact raw protocol bytes")).toHaveTextContent("secret");
    expect(screen.getByRole("status")).toHaveTextContent("truncated at the hard capture limit");
    expect(screen.queryByText(/normalized-event projection/i)).not.toBeInTheDocument();
  });

  it("formats at most one bounded raw page and exposes explicit byte paging", async () => {
    const data = new Array<number>(1024 * 1024).fill(0x41);
    data.splice(16 * 1024, 6, ...new TextEncoder().encode("SECOND"));
    const user = userEvent.setup();
    render(panel(structured({
      rawCaptureEnabled: true,
      rawProtocol: {
        capturedBytes: data.length,
        complete: true,
        data,
        nextOffset: data.length,
        observedBytes: data.length,
        runId: "run-1",
        sessionId: "session-1",
        truncated: false,
      },
      run: { processId: 42, runId: "run-1", sessionId: "session-1" },
    }), "raw"));

    const output = screen.getByLabelText("Sensitive exact raw protocol bytes");
    expect(output.textContent?.length).toBeLessThanOrEqual(16 * 1024);
    expect(output).not.toHaveTextContent("SECOND");
    expect(screen.getByRole("status")).toHaveTextContent("bounded 16 KiB pages");
    await user.click(screen.getByRole("button", { name: "Next bytes" }));
    expect(output).toHaveTextContent("SECOND");
  });

  it("clearly labels and launches the selected local fake-agent PTY scenario", async () => {
    const fakeTui = tui();
    const user = userEvent.setup();
    render(
      <BottomPanel
        activeSurface="agent"
        daemon={{
          detail: "Connected",
          status: "connected",
          storageSchemaVersion: 2,
          storageStatus: "ready",
        }}
        fakeSession={structured()}
        fakeTui={fakeTui}
        onClose={vi.fn()}
        onSelectSurface={vi.fn()}
        open
        shellTerminal={{} as ShellTerminalController}
      />,
    );

    expect(screen.getByText(/local fake agent, not Codex, Claude Code, or agy/)).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("Fake TUI scenario"), "tui/alternate-screen");
    await user.click(screen.getByRole("button", { name: "Start Exact Fake TUI" }));

    expect(fakeTui.start).toHaveBeenCalledWith("tui/alternate-screen");
  });

  it("reattaches active persisted structured and exact-TUI sessions", async () => {
    const fakeTui = tui();
    fakeTui.sessions = [
      {
        ...fakeTui.sessions[0],
        activeRunId: "persisted-structured-run",
        state: "running",
      },
      {
        activeRunId: "persisted-tui-run",
        agentKind: "fake",
        integrationMode: "pty_tui",
        latestSequence: 2,
        projectId: "persisted-project",
        sessionId: "persisted-tui-session",
        state: "running",
        title: "Fake TUI · tui/vt-baseline",
        updatedAt: "2026-08-05T10:01:00Z",
      },
    ];
    const fakeStructured = structured({ sessions: fakeTui.sessions });
    const user = userEvent.setup();
    render(
      <BottomPanel
        activeSurface="agent"
        daemon={{
          detail: "Connected",
          status: "connected",
          storageSchemaVersion: 2,
          storageStatus: "ready",
        }}
        fakeSession={fakeStructured}
        fakeTui={fakeTui}
        onClose={vi.fn()}
        onSelectSurface={vi.fn()}
        open
        shellTerminal={{} as ShellTerminalController}
      />,
    );

    expect(screen.getByRole("region", { name: "Persisted project sessions" })).toHaveTextContent("Fake · structured/happy");
    await user.click(screen.getByRole("button", { name: "Attach Fake · structured/happy" }));
    expect(fakeStructured.attach).toHaveBeenCalledWith("persisted-session");
    await user.click(screen.getByRole("button", { name: "Attach Fake TUI · tui/vt-baseline" }));
    expect(fakeTui.attach).toHaveBeenCalledWith("persisted-tui-session");
  });

  it("offers explicit session stop without routing termination through the terminal", async () => {
    const fakeTui = tui();
    fakeTui.phase = "running";
    fakeTui.sessionId = "tui-session-1";
    fakeTui.transport = {
      detach: vi.fn(),
      resize: vi.fn(),
      startPolling: vi.fn(),
      subscribe: vi.fn(() => vi.fn()),
      subscribeStatus: vi.fn(() => vi.fn()),
      write: vi.fn(),
    };
    const user = userEvent.setup();
    render(
      <BottomPanel
        activeSurface="agent"
        daemon={{
          detail: "Connected",
          status: "connected",
          storageSchemaVersion: 2,
          storageStatus: "ready",
        }}
        fakeSession={structured()}
        fakeTui={fakeTui}
        onClose={vi.fn()}
        onSelectSurface={vi.fn()}
        open
        shellTerminal={{} as ShellTerminalController}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Stop Fake TUI" }));

    expect(fakeTui.stop).toHaveBeenCalledOnce();
  });
});
