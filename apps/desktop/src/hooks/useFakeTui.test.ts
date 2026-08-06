import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { FakeSessionClient, SessionIndexEntry } from "../lib/fakeSession";
import type { TerminalCommandClient } from "../lib/terminalClient";
import {
  useFakeTui,
  type FakeTuiTransport,
  type FakeTuiTransportFactory,
} from "./useFakeTui";

const indexedSession: SessionIndexEntry = {
  activeRunId: null,
  agentKind: "fake",
  integrationMode: "structured",
  latestSequence: 9,
  projectId: "persisted-project-1",
  sessionId: "structured-session-1",
  state: "completed",
  title: "Fake · structured/happy",
  updatedAt: "2026-08-05T10:00:00Z",
};

function harness() {
  const attachTui = vi.fn().mockResolvedValue({
    sessionId: "persisted-tui-session-1",
    terminal: {
      canonicalCwd: "/tmp/project",
      processId: 92,
      runId: "persisted-tui-run-1",
      state: "running",
      terminalId: "persisted-tui-terminal-1",
    },
  });
  const startTui = vi.fn().mockResolvedValue({
    sessionId: "tui-session-1",
    terminal: {
      canonicalCwd: "/tmp/project",
      processId: 91,
      runId: "tui-run-1",
      state: "running",
      terminalId: "tui-terminal-1",
    },
  });
  const listSessions = vi.fn().mockResolvedValue([indexedSession]);
  const stop = vi.fn().mockResolvedValue(undefined);
  const fakeClient = {
    attachTui,
    listSessions,
    readEvents: vi.fn(),
    readRaw: vi.fn(),
    respondPermission: vi.fn(),
    respondUserInput: vi.fn(),
    resume: vi.fn(),
    sendGuiAction: vi.fn(),
    snapshot: vi.fn(),
    start: vi.fn(),
    startTui,
    stop,
    subscribe: vi.fn(),
    unsubscribe: vi.fn(),
  } as unknown as FakeSessionClient;
  const terminalClose = vi.fn();
  const terminalClient = {
    close: terminalClose,
    open: vi.fn(),
    read: vi.fn(),
    resize: vi.fn(),
    state: vi.fn(),
    write: vi.fn(),
  } as unknown as TerminalCommandClient;
  const detach = vi.fn();
  const startPolling = vi.fn();
  const unsubscribeStatus = vi.fn();
  const subscribeStatus: FakeTuiTransport["subscribeStatus"] = (listener) => {
    listener({
      droppedThroughSequence: null,
      error: null,
      exit: null,
      overflowed: false,
      replayTruncated: false,
      state: "running",
    });
    return unsubscribeStatus;
  };
  const transport: FakeTuiTransport = {
    detach,
    resize: vi.fn(),
    startPolling,
    subscribe: vi.fn(() => vi.fn()),
    subscribeStatus,
    write: vi.fn(),
  };
  const createTransport: FakeTuiTransportFactory = vi.fn(() => transport);
  return {
    attachTui,
    createTransport,
    detach,
    fakeClient,
    listSessions,
    startPolling,
    startTui,
    stop,
    terminalClient,
    terminalClose,
    transport,
    unsubscribeStatus,
  };
}

describe("useFakeTui", () => {
  it("loads the persisted project session index without attaching automatically", async () => {
    const test = harness();
    const hook = renderHook(() => useFakeTui(
      "project-grant",
      test.fakeClient,
      test.terminalClient,
      test.createTransport,
    ));

    await waitFor(() => expect(test.listSessions).toHaveBeenCalledWith("project-grant", 50));
    await waitFor(() => expect(hook.result.current.sessions).toEqual([indexedSession]));
    expect(hook.result.current.transport).toBeNull();

    hook.unmount();
  });

  it("reattaches an existing exact TUI and only detaches its view on unmount", async () => {
    const test = harness();
    const hook = renderHook(() => useFakeTui(
      "project-grant",
      test.fakeClient,
      test.terminalClient,
      test.createTransport,
    ));

    await act(() => hook.result.current.attach("persisted-tui-session-1"));

    expect(test.attachTui).toHaveBeenCalledWith("persisted-tui-session-1");
    expect(test.startTui).not.toHaveBeenCalled();
    expect(test.startPolling).toHaveBeenCalledOnce();
    expect(hook.result.current.sessionId).toBe("persisted-tui-session-1");
    expect(hook.result.current.transport).toBe(test.transport);
    hook.unmount();

    expect(test.unsubscribeStatus).toHaveBeenCalledOnce();
    expect(test.detach).toHaveBeenCalledOnce();
    expect(test.stop).not.toHaveBeenCalled();
    expect(test.terminalClose).not.toHaveBeenCalled();
  });

  it("starts an exact fake PTY transport and only detaches it on unmount", async () => {
    const test = harness();
    const hook = renderHook(() => useFakeTui(
      "project-grant",
      test.fakeClient,
      test.terminalClient,
      test.createTransport,
    ));

    await act(() => hook.result.current.start("tui/alternate-screen"));

    expect(test.startTui).toHaveBeenCalledWith("project-grant", "tui/alternate-screen", 100, 30);
    expect(test.startPolling).toHaveBeenCalledOnce();
    expect(hook.result.current.transport).toBe(test.transport);
    expect(hook.result.current.phase).toBe("running");
    hook.unmount();

    expect(test.unsubscribeStatus).toHaveBeenCalledOnce();
    expect(test.detach).toHaveBeenCalledOnce();
    expect(test.stop).not.toHaveBeenCalled();
    expect(test.terminalClose).not.toHaveBeenCalled();
  });

  it("terminates through session_stop only after an explicit stop action", async () => {
    const test = harness();
    const hook = renderHook(() => useFakeTui(
      "project-grant",
      test.fakeClient,
      test.terminalClient,
      test.createTransport,
    ));
    await act(() => hook.result.current.start("tui/vt-baseline"));

    await act(() => hook.result.current.stop());

    expect(test.stop).toHaveBeenCalledWith("tui-session-1");
    expect(test.terminalClose).not.toHaveBeenCalled();
    expect(test.detach).toHaveBeenCalledOnce();
    expect(hook.result.current.phase).toBe("stopped");
    expect(hook.result.current.transport).toBeNull();
    hook.unmount();
  });
});
