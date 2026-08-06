import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { EventEnvelope, FakeSessionClient, SessionEventBatch } from "../lib/fakeSession";
import { useFakeSession } from "./useFakeSession";

const permissionEvent: EventEnvelope = {
  event_id: "event-permission",
  event: {
    kind: "permission_request",
    payload: { request_id: "permission-1" },
    raw_segment_reference: null,
    vendor_event_id: null,
    visibility: "user",
  },
  run_id: "run-1",
  sequence: 1,
  session_id: "session-1",
  source: "cli",
  timestamp: "2026-08-05T00:00:00Z",
};

const inputEvent: EventEnvelope = {
  ...permissionEvent,
  event_id: "event-input",
  event: {
    ...permissionEvent.event,
    kind: "user_input_request",
    payload: { request_id: "input-1" },
  },
  sequence: 2,
};

const permissionResultEvent: EventEnvelope = {
  ...permissionEvent,
  event_id: "event-permission-result",
  event: {
    ...permissionEvent.event,
    kind: "permission_result",
  },
  sequence: 3,
};

function client(readEvents: FakeSessionClient["readEvents"]): FakeSessionClient {
  return {
    attach: vi.fn().mockResolvedValue({ processId: 1, runId: "run-1", sessionId: "session-1" }),
    attachTui: vi.fn(),
    listSessions: vi.fn().mockResolvedValue([]),
    readEvents,
    readRaw: vi.fn().mockResolvedValue({
      capturedBytes: 0,
      complete: false,
      data: [],
      nextOffset: 0,
      observedBytes: 0,
      runId: "run-1",
      sessionId: "session-1",
      truncated: false,
    }),
    respondPermission: vi.fn().mockResolvedValue(undefined),
    respondUserInput: vi.fn().mockResolvedValue(undefined),
    resume: vi.fn().mockResolvedValue({ processId: 2, runId: "run-2", sessionId: "session-1" }),
    sendGuiAction: vi.fn().mockResolvedValue("action-1"),
    snapshot: vi.fn().mockResolvedValue({
      activeRunId: null,
      binding: null,
      droppedThroughSequence: 0,
      lastError: null,
      lastExit: null,
      latestSequence: 0,
      sessionId: "session-1",
      state: "completed",
      stderr: "",
      stderrTruncated: false,
    }),
    start: vi.fn().mockResolvedValue({ processId: 1, runId: "run-1", sessionId: "session-1" }),
    startTui: vi.fn().mockResolvedValue({
      sessionId: "session-tui-1",
      terminal: {
        canonicalCwd: "/tmp/project",
        processId: 3,
        runId: "run-tui-1",
        state: "running",
        terminalId: "terminal-tui-1",
      },
    }),
    stop: vi.fn().mockResolvedValue(undefined),
    subscribe: vi.fn().mockResolvedValue(undefined),
    unsubscribe: vi.fn().mockResolvedValue(undefined),
  };
}

function pendingRead(signal?: AbortSignal): Promise<SessionEventBatch> {
  return new Promise((_resolve, reject) => {
    signal?.addEventListener("abort", () => {
      const error = new Error("cancelled");
      error.name = "AbortError";
      reject(error);
    }, { once: true });
  });
}

describe("useFakeSession", () => {
  it("attaches to an active structured run and replays without launching or resuming", async () => {
    const readEvents = vi.fn().mockResolvedValue({
      events: [],
      latestSequence: 4,
      nextSequence: 4,
      replayGap: null,
      sessionId: "session-1",
      state: "completed",
    } satisfies SessionEventBatch);
    const fakeClient = client(readEvents);
    const attach = vi.spyOn(fakeClient, "attach");
    const start = vi.spyOn(fakeClient, "start");
    const resume = vi.spyOn(fakeClient, "resume");
    const subscribe = vi.spyOn(fakeClient, "subscribe");
    const hook = renderHook(() => useFakeSession("project-grant", fakeClient));

    await act(() => hook.result.current.attach("session-1"));

    expect(attach).toHaveBeenCalledWith("project-grant", "session-1");
    expect(start).not.toHaveBeenCalled();
    expect(resume).not.toHaveBeenCalled();
    await waitFor(() => expect(subscribe).toHaveBeenCalledWith("session-1", 0));
    await waitFor(() => expect(hook.result.current.run).toMatchObject({
      runId: "run-1",
      sessionId: "session-1",
    }));
    hook.unmount();
  });

  it("opts in before launch and accumulates exact raw pages separately from events", async () => {
    const readEvents = vi.fn().mockResolvedValue({
      events: [],
      latestSequence: 2,
      nextSequence: 2,
      replayGap: null,
      sessionId: "session-1",
      state: "completed",
    } satisfies SessionEventBatch);
    const fakeClient = client(readEvents);
    const start = vi.spyOn(fakeClient, "start");
    fakeClient.readRaw = vi.fn().mockResolvedValue({
      capturedBytes: 4,
      complete: true,
      data: [0x41, 0x0a, 0xff, 0x42],
      nextOffset: 4,
      observedBytes: 9,
      runId: "run-1",
      sessionId: "session-1",
      truncated: true,
    });
    const hook = renderHook(() => useFakeSession("project-grant", fakeClient, true));

    act(() => hook.result.current.setRawCaptureEnabled(true));
    await act(() => hook.result.current.start("structured/happy"));

    expect(start).toHaveBeenCalledWith(
      "project-grant",
      "structured/happy",
      null,
      null,
      true,
    );
    await waitFor(() => expect(hook.result.current.rawProtocol?.complete).toBe(true));
    expect(hook.result.current.rawProtocol).toMatchObject({
      capturedBytes: 4,
      data: [0x41, 0x0a, 0xff, 0x42],
      observedBytes: 9,
      truncated: true,
    });
  });

  it("fetches and retains sensitive raw bytes only while the Raw view is active", async () => {
    const readEvents = vi.fn().mockResolvedValue({
      events: [],
      latestSequence: 2,
      nextSequence: 2,
      replayGap: null,
      sessionId: "session-1",
      state: "completed",
    } satisfies SessionEventBatch);
    const fakeClient = client(readEvents);
    const readRaw = vi.spyOn(fakeClient, "readRaw").mockResolvedValue({
      capturedBytes: 2,
      complete: true,
      data: [0x41, 0x42],
      nextOffset: 2,
      observedBytes: 2,
      runId: "run-1",
      sessionId: "session-1",
      truncated: false,
    });
    const hook = renderHook(
      ({ rawActive }) => useFakeSession("project-grant", fakeClient, rawActive),
      { initialProps: { rawActive: false } },
    );

    act(() => hook.result.current.setRawCaptureEnabled(true));
    await act(() => hook.result.current.start("structured/happy"));
    await waitFor(() => expect(hook.result.current.state).toBe("completed"));
    expect(readRaw).not.toHaveBeenCalled();
    expect(hook.result.current.rawProtocol).toBeNull();

    hook.rerender({ rawActive: true });
    await waitFor(() => expect(hook.result.current.rawProtocol?.data).toEqual([0x41, 0x42]));
    hook.rerender({ rawActive: false });
    await waitFor(() => expect(hook.result.current.rawProtocol).toBeNull());
  });

  it("aborts its active long poll and unsubscribes without stopping on unmount", async () => {
    let pollingSignal: AbortSignal | undefined;
    const readEvents = vi.fn((_sessionId: string, _after: number, signal?: AbortSignal) => {
      pollingSignal = signal;
      return new Promise<SessionEventBatch>((_resolve, reject) => {
        signal?.addEventListener("abort", () => {
          const error = new Error("cancelled");
          error.name = "AbortError";
          reject(error);
        }, { once: true });
      });
    });
    const fakeClient = client(readEvents);
    const subscribe = vi.fn().mockResolvedValue(undefined);
    const unsubscribe = vi.fn().mockResolvedValue(undefined);
    const stop = vi.fn().mockResolvedValue(undefined);
    fakeClient.subscribe = subscribe;
    fakeClient.unsubscribe = unsubscribe;
    fakeClient.stop = stop;
    const hook = renderHook(() => useFakeSession("project-grant", fakeClient));

    await act(() => hook.result.current.start("structured/stall"));
    await waitFor(() => expect(subscribe).toHaveBeenCalledWith("session-1", 0));
    await waitFor(() => expect(readEvents).toHaveBeenCalledOnce());
    expect(pollingSignal?.aborted).toBe(false);

    hook.unmount();

    expect(pollingSignal?.aborted).toBe(true);
    await waitFor(() => expect(unsubscribe).toHaveBeenCalledWith("session-1"));
    expect(stop).not.toHaveBeenCalled();
  });

  it("reports rejected permission delivery and allows a successful retry", async () => {
    let resolveResultBatch: ((batch: SessionEventBatch) => void) | undefined;
    const readEvents = vi.fn()
      .mockResolvedValueOnce({
        events: [permissionEvent],
        latestSequence: 1,
        nextSequence: 1,
        replayGap: null,
        sessionId: "session-1",
        state: "awaiting_permission",
      } satisfies SessionEventBatch)
      .mockImplementationOnce((_sessionId: string, _after: number, signal?: AbortSignal) => new Promise<SessionEventBatch>((resolve, reject) => {
        resolveResultBatch = resolve;
        signal?.addEventListener("abort", () => {
          const error = new Error("cancelled");
          error.name = "AbortError";
          reject(error);
        }, { once: true });
      }))
      .mockImplementation((_sessionId: string, _after: number, signal?: AbortSignal) => pendingRead(signal));
    const fakeClient = client(readEvents);
    const respondPermission = vi.fn()
      .mockRejectedValueOnce({
        details: { delivery: "not_delivered", retry_safe: true },
        message: "transport rejected",
      })
      .mockResolvedValueOnce(undefined);
    fakeClient.respondPermission = respondPermission;
    const hook = renderHook(() => useFakeSession("project-grant", fakeClient));

    await act(() => hook.result.current.start("structured/permission"));
    await waitFor(() => expect(hook.result.current.pendingPermission?.requestId).toBe("permission-1"));

    let firstDelivered: boolean | undefined;
    await act(async () => {
      firstDelivered = await hook.result.current.respondPermission(
        hook.result.current.pendingPermission!,
        "allow",
      );
    });
    expect(firstDelivered).toBe(false);
    expect(hook.result.current.resolvedRequestIds.has("permission-1")).toBe(false);

    let retryDelivered: boolean | undefined;
    await act(async () => {
      retryDelivered = await hook.result.current.respondPermission(
        hook.result.current.pendingPermission!,
        "allow",
      );
    });
    expect(retryDelivered).toBe(true);
    expect(respondPermission).toHaveBeenCalledTimes(2);
    expect(hook.result.current.resolvedRequestIds.has("permission-1")).toBe(true);

    act(() => {
      resolveResultBatch?.({
        events: [permissionResultEvent],
        latestSequence: 3,
        nextSequence: 3,
        replayGap: null,
        sessionId: "session-1",
        state: "ready",
      });
    });
    await waitFor(() => expect(hook.result.current.pendingPermission).toBeNull());
    expect(hook.result.current.resolvedRequestIds.has("permission-1")).toBe(false);
    hook.unmount();
  });

  it("keeps an ambiguous permission delivery disabled until the event stream resolves it", async () => {
    const readEvents = vi.fn()
      .mockResolvedValueOnce({
        events: [permissionEvent],
        latestSequence: 1,
        nextSequence: 1,
        replayGap: null,
        sessionId: "session-1",
        state: "awaiting_permission",
      } satisfies SessionEventBatch)
      .mockImplementation((_sessionId: string, _after: number, signal?: AbortSignal) => pendingRead(signal));
    const fakeClient = client(readEvents);
    const respondPermission = vi.fn().mockRejectedValue({
      details: { delivery: "uncertain", retry_safe: false },
      message: "delivery outcome is uncertain",
    });
    fakeClient.respondPermission = respondPermission;
    const hook = renderHook(() => useFakeSession("project-grant", fakeClient));

    await act(() => hook.result.current.start("structured/permission"));
    await waitFor(() => expect(hook.result.current.pendingPermission?.requestId).toBe("permission-1"));
    const request = hook.result.current.pendingPermission!;

    let firstDelivered: boolean | undefined;
    let retryDelivered: boolean | undefined;
    await act(async () => {
      firstDelivered = await hook.result.current.respondPermission(request, "allow");
      retryDelivered = await hook.result.current.respondPermission(request, "allow");
    });

    expect(firstDelivered).toBe(false);
    expect(retryDelivered).toBe(false);
    expect(respondPermission).toHaveBeenCalledOnce();
    expect(hook.result.current.resolvedRequestIds.has("permission-1")).toBe(true);
    expect(hook.result.current.error).toMatch(/may have reached the CLI/i);
    hook.unmount();
  });

  it("rejects a duplicate user-input delivery while the first call is in flight", async () => {
    const readEvents = vi.fn()
      .mockResolvedValueOnce({
        events: [inputEvent],
        latestSequence: 2,
        nextSequence: 2,
        replayGap: null,
        sessionId: "session-1",
        state: "awaiting_user_input",
      } satisfies SessionEventBatch)
      .mockImplementation((_sessionId: string, _after: number, signal?: AbortSignal) => pendingRead(signal));
    const fakeClient = client(readEvents);
    let finishDelivery: (() => void) | undefined;
    const respondUserInput = vi.fn(() => new Promise<void>((resolve) => {
      finishDelivery = resolve;
    }));
    fakeClient.respondUserInput = respondUserInput;
    const hook = renderHook(() => useFakeSession("project-grant", fakeClient));

    await act(() => hook.result.current.start("structured/user-input"));
    await waitFor(() => expect(hook.result.current.pendingInput?.requestId).toBe("input-1"));

    let firstDelivery: Promise<boolean> | undefined;
    let duplicateDelivery: boolean | undefined;
    await act(async () => {
      const request = hook.result.current.pendingInput!;
      firstDelivery = hook.result.current.respondUserInput(request, "sensitive response");
      duplicateDelivery = await hook.result.current.respondUserInput(request, "duplicate");
    });
    expect(duplicateDelivery).toBe(false);
    expect(respondUserInput).toHaveBeenCalledOnce();

    await act(async () => {
      finishDelivery?.();
      await firstDelivery;
    });
    expect(await firstDelivery).toBe(true);
    hook.unmount();
  });

  it("propagates the final daemon snapshot after a failed event stream settles", async () => {
    const readEvents = vi.fn().mockResolvedValue({
      events: [],
      latestSequence: 3,
      nextSequence: 3,
      replayGap: null,
      sessionId: "session-1",
      state: "failed",
    } satisfies SessionEventBatch);
    const fakeClient = client(readEvents);
    const finalSnapshot = {
      activeRunId: null,
      binding: "vendor-session-1",
      droppedThroughSequence: 0,
      lastError: {
        code: "process_crashed",
        correlationId: "11111111-2222-4333-8444-555555555555",
        message: "the fake CLI process exited unsuccessfully",
      },
      lastExit: { cause: "exited", value: 17 },
      latestSequence: 3,
      sessionId: "session-1",
      state: "failed" as const,
      stderr: "bounded stderr",
      stderrTruncated: true,
    };
    fakeClient.snapshot = vi.fn().mockResolvedValue(finalSnapshot);
    const hook = renderHook(() => useFakeSession("project-grant", fakeClient));

    await act(() => hook.result.current.start("structured/nonzero"));

    await waitFor(() => expect(hook.result.current.snapshot).toEqual(finalSnapshot));
    expect(hook.result.current.state).toBe("failed");
    hook.unmount();
  });
});
