import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createTauriTerminalCommandClient,
  DaemonTerminalTransport,
  type TerminalCommandClient,
  describeTerminalError,
  type TerminalOpened,
  type TerminalReadResult,
  type TerminalStatus,
} from "./terminalClient";

const opened: TerminalOpened = {
  canonicalCwd: "/projects/maestro-app",
  processId: 451,
  runId: "run-terminal-1",
  state: "running",
  terminalId: "terminal-1",
};

function readResult(update: Partial<TerminalReadResult> = {}): TerminalReadResult {
  return {
    chunks: [],
    droppedThroughSequence: null,
    exit: null,
    latestSequence: 0,
    nextSequence: 0,
    overflowed: false,
    state: "running",
    terminalId: opened.terminalId,
    ...update,
  };
}

function commandClient(read: TerminalCommandClient["read"]): TerminalCommandClient {
  return {
    attach: vi.fn(() => Promise.resolve(opened)),
    close: vi.fn<TerminalCommandClient["close"]>((terminalId) => Promise.resolve({ exit: null, state: "closed", terminalId })),
    list: vi.fn().mockResolvedValue([]),
    open: vi.fn(() => Promise.resolve(opened)),
    read,
    resize: vi.fn<TerminalCommandClient["resize"]>((terminalId) => Promise.resolve({ terminalId })),
    state: vi.fn<TerminalCommandClient["state"]>((terminalId): Promise<TerminalStatus> => Promise.resolve({ exit: null, state: "running", terminalId })),
    write: vi.fn<TerminalCommandClient["write"]>((terminalId) => Promise.resolve({ terminalId })),
  };
}

describe("Tauri terminal command client", () => {
  it("uses the exact command names and camelCase argument shapes", async () => {
    const calls: unknown[][] = [];
    const invokeCommand = <T>(command: string, args?: Record<string, unknown>) => {
      calls.push([command, args]);
      return Promise.resolve({ terminalId: "terminal-1" } as T);
    };
    const client = createTauriTerminalCommandClient(invokeCommand);

    await client.list("project-grant-1", 12);
    await client.attach("project-grant-1", "terminal-reference-1");
    await client.open("project-grant-1", 100, 30);
    await client.write("terminal-1", new Uint8Array([108, 115, 13]));
    await client.resize("terminal-1", 120, 40);
    await client.read("terminal-1", 9, 65_536);
    await client.state("terminal-1");
    await client.close("terminal-1");

    expect(calls).toEqual([
      ["terminal_list", { maximumTerminals: 12, projectGrant: "project-grant-1" }],
      ["terminal_attach", { projectGrant: "project-grant-1", terminalId: "terminal-reference-1" }],
      ["terminal_open", { columns: 100, projectGrant: "project-grant-1", rows: 30 }],
      ["terminal_write", { data: [108, 115, 13], terminalId: "terminal-1" }],
      ["terminal_resize", { columns: 120, rows: 40, terminalId: "terminal-1" }],
      ["terminal_read", { afterSequence: 9, maximumBytes: 65_536, terminalId: "terminal-1" }],
      ["terminal_state", { terminalId: "terminal-1" }],
      ["terminal_close", { terminalId: "terminal-1" }],
    ]);
  });

  it("preserves the safe message from a structured native command error", () => {
    expect(describeTerminalError({
      code: "PERMISSION_DENIED",
      correlationId: "correlation-1",
      details: null,
      message: "The terminal is not available to this window.",
      retryable: false,
      userAction: null,
    })).toBe("The terminal is not available to this window.");
  });
});

describe("DaemonTerminalTransport", () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
  afterEach(() => vi.useRealTimers());

  it("allows exactly one terminal read in flight", async () => {
    let resolveRead: ((result: TerminalReadResult) => void) | undefined;
    const read = vi.fn(() => new Promise<TerminalReadResult>((resolve) => { resolveRead = resolve; }));
    const transport = new DaemonTerminalTransport(commandClient(read), opened, { pollDelayMilliseconds: 20 });

    transport.startPolling();
    await vi.advanceTimersByTimeAsync(0);
    expect(read).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(500);
    expect(read).toHaveBeenCalledOnce();

    resolveRead?.(readResult({ state: "exited" }));
    await Promise.resolve();
    transport.detach();
  });

  it("preserves source ordering when the first terminal write is delayed", async () => {
    let finishFirst: (() => void) | undefined;
    const delivered: string[] = [];
    const client = commandClient(vi.fn(() => new Promise<TerminalReadResult>(() => undefined)));
    client.write = vi.fn<TerminalCommandClient["write"]>((_terminalId, data) => {
      const text = new TextDecoder().decode(Uint8Array.from(data));
      delivered.push(text);
      if (text === "first") return new Promise((resolve) => { finishFirst = () => resolve({ terminalId: opened.terminalId }); });
      return Promise.resolve({ terminalId: opened.terminalId });
    });
    const transport = new DaemonTerminalTransport(client, opened);

    const first = transport.write("first");
    const second = transport.write("second");
    await Promise.resolve();
    expect(delivered).toEqual(["first"]);

    finishFirst?.();
    await Promise.all([first, second]);
    expect(delivered).toEqual(["first", "second"]);
  });

  it("pauses daemon reads at the renderer high-water mark until xterm acknowledges parsing", async () => {
    const read = vi.fn()
      .mockResolvedValueOnce(readResult({
        chunks: [{ data: [65, 66, 67, 68], sequence: 1 }],
        latestSequence: 2,
        nextSequence: 1,
      }))
      .mockResolvedValueOnce(readResult({
        exit: { code: 0, signal: null },
        latestSequence: 2,
        nextSequence: 2,
        state: "exited",
      }));
    let acknowledge: (() => void) | undefined;
    const transport = new DaemonTerminalTransport(commandClient(read), opened, {
      pollDelayMilliseconds: 10,
      rendererHighWaterBytes: 4,
    });
    transport.subscribe(() => new Promise<void>((resolve) => { acknowledge = resolve; }));

    transport.startPolling();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(read).toHaveBeenCalledOnce();

    acknowledge?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("passes the daemon cursor back unchanged and suppresses duplicate chunks", async () => {
    const read = vi.fn()
      .mockResolvedValueOnce(readResult({
        chunks: [
          { data: [65], sequence: 1 },
          { data: [88], sequence: 1 },
          { data: [66], sequence: 2 },
        ],
        latestSequence: 2,
        nextSequence: 2,
      }))
      .mockResolvedValueOnce(readResult({
        exit: { code: 0, signal: null },
        latestSequence: 2,
        nextSequence: 2,
        state: "exited",
      }));
    const transport = new DaemonTerminalTransport(commandClient(read), opened, { pollDelayMilliseconds: 20 });
    const output: number[] = [];
    transport.subscribe((data) => {
      if (data instanceof Uint8Array) output.push(...data);
    });

    transport.startPolling();
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(20);

    expect(read).toHaveBeenNthCalledWith(1, "terminal-1", 0, 65_536);
    expect(read).toHaveBeenNthCalledWith(2, "terminal-1", 2, 65_536);
    expect(output).toEqual([65, 66]);
  });

  it("publishes daemon overflow and local replay truncation notices", async () => {
    const read = vi.fn().mockResolvedValue(readResult({
      chunks: [{ data: [65, 66, 67, 68], sequence: 8 }],
      droppedThroughSequence: 7,
      exit: { code: 0, signal: null },
      latestSequence: 8,
      nextSequence: 8,
      overflowed: true,
      state: "exited",
    }));
    const transport = new DaemonTerminalTransport(commandClient(read), opened, { replayBytes: 2 });
    const snapshots: boolean[][] = [];
    transport.subscribeStatus((snapshot) => snapshots.push([snapshot.overflowed, snapshot.replayTruncated]));

    transport.startPolling();
    await vi.advanceTimersByTimeAsync(0);

    expect(snapshots).toContainEqual([true, true]);
    const replay: number[] = [];
    transport.subscribe((data) => {
      if (data instanceof Uint8Array) replay.push(...data);
    });
    expect(replay).toEqual([67, 68]);
  });

  it("detaches without closing and closes only when explicitly requested", async () => {
    const client = commandClient(vi.fn(() => new Promise<TerminalReadResult>(() => undefined)));
    const detached = new DaemonTerminalTransport(client, opened);
    detached.startPolling();
    await vi.advanceTimersByTimeAsync(0);
    detached.detach();
    expect(client.close).not.toHaveBeenCalled();

    const stopped = new DaemonTerminalTransport(client, opened);
    await stopped.close();
    expect(client.close).toHaveBeenCalledOnce();
    expect(client.close).toHaveBeenCalledWith("terminal-1");
  });
});
