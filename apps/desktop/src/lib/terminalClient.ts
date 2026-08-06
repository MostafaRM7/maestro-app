import { invoke } from "@tauri-apps/api/core";
import type { TerminalTransport } from "./terminal";

export type TerminalState = "running" | "closing" | "exited" | "failed" | "closed";

export interface TerminalExit {
  code: number | null;
  signal: number | null;
}

export interface TerminalOpened {
  terminalId: string;
  runId: string;
  processId: number;
  canonicalCwd: string;
  state: TerminalState;
}

export interface TerminalIndexEntry extends TerminalOpened {
  exit: TerminalExit | null;
  kind: string;
  title: string;
}

export interface TerminalOutputChunk {
  sequence: number;
  data: number[];
}

export interface TerminalReadResult {
  terminalId: string;
  chunks: TerminalOutputChunk[];
  nextSequence: number;
  latestSequence: number;
  overflowed: boolean;
  droppedThroughSequence: number | null;
  state: TerminalState;
  exit: TerminalExit | null;
}

export interface TerminalStatus {
  terminalId: string;
  state: TerminalState;
  exit: TerminalExit | null;
}

export interface TerminalAcknowledgement {
  terminalId: string;
}

export interface TerminalCommandError {
  code: string;
  correlationId: string;
  details: unknown;
  message: string;
  retryable: boolean;
  userAction: string | null;
}

export interface TerminalCommandClient {
  attach: (projectGrant: string, terminalId: string) => Promise<TerminalOpened>;
  close: (terminalId: string) => Promise<TerminalStatus>;
  list: (projectGrant: string, maximumTerminals?: number) => Promise<TerminalIndexEntry[]>;
  open: (projectGrant: string, columns: number, rows: number) => Promise<TerminalOpened>;
  read: (terminalId: string, afterSequence: number, maximumBytes: number) => Promise<TerminalReadResult>;
  resize: (terminalId: string, columns: number, rows: number) => Promise<TerminalAcknowledgement>;
  state: (terminalId: string) => Promise<TerminalStatus>;
  write: (terminalId: string, data: Uint8Array | readonly number[]) => Promise<TerminalAcknowledgement>;
}

type InvokeCommand = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export function createTauriTerminalCommandClient(invokeCommand: InvokeCommand = invoke): TerminalCommandClient {
  return {
    attach(projectGrant, terminalId) {
      return invokeCommand<TerminalOpened>("terminal_attach", { projectGrant, terminalId });
    },
    close(terminalId) {
      return invokeCommand<TerminalStatus>("terminal_close", { terminalId });
    },
    open(projectGrant, columns, rows) {
      return invokeCommand<TerminalOpened>("terminal_open", { columns, projectGrant, rows });
    },
    list(projectGrant, maximumTerminals = 32) {
      return invokeCommand<TerminalIndexEntry[]>("terminal_list", { maximumTerminals, projectGrant });
    },
    read(terminalId, afterSequence, maximumBytes) {
      return invokeCommand<TerminalReadResult>("terminal_read", { afterSequence, maximumBytes, terminalId });
    },
    resize(terminalId, columns, rows) {
      return invokeCommand<TerminalAcknowledgement>("terminal_resize", { columns, rows, terminalId });
    },
    state(terminalId) {
      return invokeCommand<TerminalStatus>("terminal_state", { terminalId });
    },
    write(terminalId, data) {
      return invokeCommand<TerminalAcknowledgement>("terminal_write", { data: Array.from(data), terminalId });
    },
  };
}

export const tauriTerminalCommandClient = createTauriTerminalCommandClient();

export type TerminalConnectionState = TerminalState | "disconnected";

export interface TerminalConnectionSnapshot {
  droppedThroughSequence: number | null;
  error: string | null;
  exit: TerminalExit | null;
  overflowed: boolean;
  replayTruncated: boolean;
  state: TerminalConnectionState;
}

interface DaemonTerminalTransportOptions {
  maximumReadBytes?: number;
  pollDelayMilliseconds?: number;
  rendererHighWaterBytes?: number;
  replayBytes?: number;
}

const DEFAULT_MAXIMUM_READ_BYTES = 64 * 1024;
const DEFAULT_POLL_DELAY_MILLISECONDS = 150;
const DEFAULT_RENDERER_HIGH_WATER_BYTES = 256 * 1024;
const DEFAULT_REPLAY_BYTES = 1024 * 1024;

/**
 * Bridges one daemon-owned terminal into xterm without taking ownership of the
 * process lifecycle. Detaching stops frontend reads; only close() asks the
 * daemon to terminate the terminal.
 */
export class DaemonTerminalTransport implements TerminalTransport {
  readonly canonicalCwd: string;
  readonly processId: number;
  readonly runId: string;
  readonly terminalId: string;

  private readonly client: TerminalCommandClient;
  private readonly maximumReadBytes: number;
  private readonly pollDelayMilliseconds: number;
  private readonly rendererHighWaterBytes: number;
  private readonly replayByteLimit: number;
  private readonly outputListeners = new Set<(data: string | Uint8Array) => void | Promise<void>>();
  private readonly statusListeners = new Set<(snapshot: TerminalConnectionSnapshot) => void>();
  private readonly replayChunks: Uint8Array[] = [];
  private readonly encoder = new TextEncoder();
  private cursor = 0;
  private closeRequested = false;
  private detached = false;
  private lastDeliveredSequence = 0;
  private operationTail: Promise<void> = Promise.resolve();
  private pendingRendererBytes = 0;
  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private readInFlight = false;
  private replayByteCount = 0;
  private snapshot: TerminalConnectionSnapshot;

  constructor(client: TerminalCommandClient, opened: TerminalOpened, options: DaemonTerminalTransportOptions = {}) {
    this.client = client;
    this.terminalId = opened.terminalId;
    this.runId = opened.runId;
    this.processId = opened.processId;
    this.canonicalCwd = opened.canonicalCwd;
    this.maximumReadBytes = options.maximumReadBytes ?? DEFAULT_MAXIMUM_READ_BYTES;
    this.pollDelayMilliseconds = options.pollDelayMilliseconds ?? DEFAULT_POLL_DELAY_MILLISECONDS;
    this.rendererHighWaterBytes = options.rendererHighWaterBytes ?? DEFAULT_RENDERER_HIGH_WATER_BYTES;
    this.replayByteLimit = options.replayBytes ?? DEFAULT_REPLAY_BYTES;
    this.snapshot = {
      droppedThroughSequence: null,
      error: null,
      exit: null,
      overflowed: false,
      replayTruncated: false,
      state: opened.state,
    };
  }

  startPolling() {
    if (this.detached || this.pollTimer || this.readInFlight || isTerminal(this.snapshot.state)) return;
    this.schedulePoll(0);
  }

  subscribe(listener: (data: string | Uint8Array) => void | Promise<void>) {
    this.outputListeners.add(listener);
    for (const chunk of this.replayChunks) this.deliver(chunk, [listener]);
    return () => this.outputListeners.delete(listener);
  }

  subscribeStatus(listener: (snapshot: TerminalConnectionSnapshot) => void) {
    listener(this.snapshot);
    this.statusListeners.add(listener);
    return () => this.statusListeners.delete(listener);
  }

  write(data: string) {
    const bytes = this.encoder.encode(data);
    return this.enqueueInteraction(async () => {
      await this.client.write(this.terminalId, bytes);
    });
  }

  resize(columns: number, rows: number) {
    return this.enqueueInteraction(async () => {
      await this.client.resize(this.terminalId, columns, rows);
    });
  }

  detach() {
    this.detached = true;
    if (this.pollTimer) clearTimeout(this.pollTimer);
    this.pollTimer = null;
    this.outputListeners.clear();
    this.statusListeners.clear();
  }

  async close() {
    if (this.pollTimer) clearTimeout(this.pollTimer);
    this.pollTimer = null;
    this.closeRequested = true;
    this.updateSnapshot({ state: "closing" });
    await this.operationTail;
    try {
      const status = await this.client.close(this.terminalId);
      this.updateSnapshot({ error: null, exit: status.exit, state: status.state });
      return status;
    } catch (reason) {
      this.disconnect(reason);
      throw reason;
    }
  }

  private canInteract() {
    return !this.detached && this.snapshot.state === "running";
  }

  private enqueueInteraction(operation: () => Promise<void>) {
    if (!this.canInteract()) return Promise.resolve();
    const queued = this.operationTail.then(async () => {
      if (!this.canInteract()) return;
      await operation();
    });
    this.operationTail = queued.catch((reason: unknown) => {
      if (!this.closeRequested && !isTerminal(this.snapshot.state)) this.disconnect(reason);
    });
    return this.operationTail;
  }

  private schedulePoll(delay: number) {
    if (this.detached || isTerminal(this.snapshot.state) || this.rendererBackpressured()) return;
    this.pollTimer = setTimeout(() => {
      this.pollTimer = null;
      void this.poll();
    }, delay);
  }

  private async poll() {
    if (this.detached || this.readInFlight || isTerminal(this.snapshot.state)) return;
    this.readInFlight = true;
    const requestedCursor = this.cursor;
    try {
      const result = await this.client.read(this.terminalId, requestedCursor, this.maximumReadBytes);
      if (this.detached || isTerminal(this.snapshot.state)) return;
      this.validateRead(result, requestedCursor);

      for (const chunk of result.chunks) {
        if (chunk.sequence <= this.lastDeliveredSequence) continue;
        const bytes = Uint8Array.from(chunk.data);
        this.appendReplay(bytes);
        this.lastDeliveredSequence = chunk.sequence;
        this.deliver(bytes, this.outputListeners);
      }

      // The daemon explicitly defines nextSequence as the next afterSequence.
      this.cursor = result.nextSequence;
      this.updateSnapshot({
        droppedThroughSequence: result.droppedThroughSequence ?? this.snapshot.droppedThroughSequence,
        error: null,
        exit: result.exit,
        overflowed: this.snapshot.overflowed || result.overflowed,
        state: this.closeRequested && result.state === "running" ? "closing" : result.state,
      });

      if (!isTerminal(result.state) && !this.rendererBackpressured()) {
        const hasBufferedOutput = result.nextSequence < result.latestSequence;
        this.schedulePoll(hasBufferedOutput ? 0 : this.pollDelayMilliseconds);
      }
    } catch (reason) {
      if (!this.detached && !this.closeRequested && !isTerminal(this.snapshot.state)) this.disconnect(reason);
    } finally {
      this.readInFlight = false;
    }
  }

  private validateRead(result: TerminalReadResult, requestedCursor: number) {
    if (result.terminalId !== this.terminalId) {
      throw new Error("The daemon returned output for a different terminal.");
    }
    if (result.nextSequence < requestedCursor) {
      throw new Error("The daemon terminal cursor moved backwards.");
    }
    if (result.latestSequence < result.nextSequence) {
      throw new Error("The daemon returned an invalid terminal cursor.");
    }
  }

  private appendReplay(bytes: Uint8Array) {
    if (bytes.byteLength === 0 || this.replayByteLimit <= 0) return;
    const retained = bytes.byteLength > this.replayByteLimit
      ? bytes.slice(bytes.byteLength - this.replayByteLimit)
      : bytes;
    if (retained.byteLength !== bytes.byteLength) this.markReplayTruncated();
    this.replayChunks.push(retained);
    this.replayByteCount += retained.byteLength;

    while (this.replayByteCount > this.replayByteLimit) {
      const removed = this.replayChunks.shift();
      if (!removed) break;
      this.replayByteCount -= removed.byteLength;
      this.markReplayTruncated();
    }
  }

  private deliver(
    bytes: Uint8Array,
    listeners: Iterable<(data: string | Uint8Array) => void | Promise<void>>,
  ) {
    const acknowledgements: Promise<void>[] = [];
    for (const listener of listeners) {
      try {
        const result = listener(bytes);
        if (result && typeof result.then === "function") acknowledgements.push(result);
      } catch {
        // A renderer subscriber cannot invalidate the daemon terminal stream.
      }
    }
    if (acknowledgements.length === 0) return;

    this.pendingRendererBytes += bytes.byteLength;
    void Promise.allSettled(acknowledgements).then(() => {
      this.pendingRendererBytes = Math.max(0, this.pendingRendererBytes - bytes.byteLength);
      if (!this.rendererBackpressured() && !this.pollTimer && !this.readInFlight) this.schedulePoll(0);
    });
  }

  private rendererBackpressured() {
    return this.pendingRendererBytes >= this.rendererHighWaterBytes;
  }

  private markReplayTruncated() {
    if (!this.snapshot.replayTruncated) this.updateSnapshot({ replayTruncated: true });
  }

  private disconnect(reason: unknown) {
    if (this.pollTimer) clearTimeout(this.pollTimer);
    this.pollTimer = null;
    this.updateSnapshot({ error: describeTerminalError(reason), state: "disconnected" });
  }

  private updateSnapshot(update: Partial<TerminalConnectionSnapshot>) {
    const next = { ...this.snapshot, ...update };
    if (snapshotsEqual(this.snapshot, next)) return;
    this.snapshot = next;
    for (const listener of this.statusListeners) listener(this.snapshot);
  }
}

function isTerminal(state: TerminalConnectionState) {
  return state === "exited" || state === "failed" || state === "closed" || state === "disconnected";
}

function snapshotsEqual(left: TerminalConnectionSnapshot, right: TerminalConnectionSnapshot) {
  return left.droppedThroughSequence === right.droppedThroughSequence
    && left.error === right.error
    && left.exit?.code === right.exit?.code
    && left.exit?.signal === right.exit?.signal
    && left.overflowed === right.overflowed
    && left.replayTruncated === right.replayTruncated
    && left.state === right.state;
}

export function describeTerminalError(reason: unknown): string {
  if (reason instanceof Error && reason.message) return reason.message;
  if (typeof reason === "string" && reason) return reason;
  if (reason && typeof reason === "object" && "message" in reason && typeof reason.message === "string") {
    return reason.message;
  }
  return "The Maestro service stopped responding to the terminal.";
}
