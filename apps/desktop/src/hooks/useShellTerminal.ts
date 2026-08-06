import { useCallback, useEffect, useRef, useState } from "react";
import {
  DaemonTerminalTransport,
  describeTerminalError,
  type TerminalCommandClient,
  type TerminalConnectionSnapshot,
  type TerminalIndexEntry,
  type TerminalOpened,
} from "../lib/terminalClient";

export type ShellTerminalPhase = "idle" | "starting" | "attaching" | "running" | "stopping" | "exited" | "failed" | "closed" | "disconnected";

export interface ShellTerminalState {
  canonicalCwd: string | null;
  droppedThroughSequence: number | null;
  error: string | null;
  exitCode: number | null;
  overflowed: boolean;
  phase: ShellTerminalPhase;
  processId: number | null;
  replayTruncated: boolean;
  runId: string | null;
  terminalId: string | null;
  transport: DaemonTerminalTransport | null;
}

export interface ShellTerminalController extends ShellTerminalState {
  attach: (terminalId: string) => Promise<void>;
  listError: string | null;
  listLoading: boolean;
  reloadTerminals: () => Promise<void>;
  start: () => Promise<void>;
  stop: () => Promise<void>;
  terminals: readonly TerminalIndexEntry[];
}

const idleState: ShellTerminalState = {
  canonicalCwd: null,
  droppedThroughSequence: null,
  error: null,
  exitCode: null,
  overflowed: false,
  phase: "idle",
  processId: null,
  replayTruncated: false,
  runId: null,
  terminalId: null,
  transport: null,
};

export function useShellTerminal(projectGrant: string, client: TerminalCommandClient): ShellTerminalController {
  const [state, setState] = useState<ShellTerminalState>(idleState);
  const [terminals, setTerminals] = useState<readonly TerminalIndexEntry[]>([]);
  const [listLoading, setListLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  const mountedRef = useRef(false);
  const operationRef = useRef(0);
  const transportRef = useRef<DaemonTerminalTransport | null>(null);
  const unsubscribeRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      operationRef.current += 1;
      unsubscribeRef.current?.();
      transportRef.current?.detach();
      unsubscribeRef.current = null;
      transportRef.current = null;
    };
  }, []);

  const reloadTerminals = useCallback(async () => {
    setListLoading(true);
    setListError(null);
    try {
      const listed = await client.list(projectGrant, 32);
      if (mountedRef.current) setTerminals(listed);
    } catch (reason) {
      if (mountedRef.current) setListError(describeTerminalError(reason));
    } finally {
      if (mountedRef.current) setListLoading(false);
    }
  }, [client, projectGrant]);

  useEffect(() => {
    void reloadTerminals();
  }, [reloadTerminals]);

  const activateOpened = useCallback((opened: TerminalOpened, operation: number) => {
    if (!mountedRef.current || operationRef.current !== operation) return;
    if (opened.state !== "running") {
      setState({
        ...idleState,
        canonicalCwd: opened.canonicalCwd,
        error: `The shell opened in an unexpected ${opened.state} state.`,
        phase: opened.state === "exited" || opened.state === "failed" || opened.state === "closed"
          ? opened.state
          : "failed",
        processId: opened.processId,
        runId: opened.runId,
        terminalId: opened.terminalId,
      });
      return;
    }

    const transport = new DaemonTerminalTransport(client, opened);
    transportRef.current = transport;
    const base = {
      canonicalCwd: opened.canonicalCwd,
      processId: opened.processId,
      runId: opened.runId,
      terminalId: opened.terminalId,
      transport,
    };
    unsubscribeRef.current = transport.subscribeStatus((snapshot) => {
      if (!mountedRef.current || transportRef.current !== transport) return;
      setState((current) => statusToState(current, base, snapshot));
    });
    transport.startPolling();
  }, [client]);

  const start = useCallback(async () => {
    const operation = operationRef.current + 1;
    operationRef.current = operation;
    const previousTransport = transportRef.current;
    unsubscribeRef.current?.();
    unsubscribeRef.current = null;
    transportRef.current = null;
    setState({ ...idleState, phase: "starting" });

    previousTransport?.detach();

    try {
      if (!mountedRef.current || operationRef.current !== operation) return;

      const opened = await client.open(projectGrant, 100, 30);
      activateOpened(opened, operation);
    } catch (reason) {
      if (!mountedRef.current || operationRef.current !== operation) return;
      setState({ ...idleState, error: describeTerminalError(reason), phase: "failed" });
    }
  }, [activateOpened, client, projectGrant]);

  const attach = useCallback(async (terminalId: string) => {
    const operation = operationRef.current + 1;
    operationRef.current = operation;
    unsubscribeRef.current?.();
    unsubscribeRef.current = null;
    transportRef.current?.detach();
    transportRef.current = null;
    setState({ ...idleState, phase: "attaching" });
    try {
      const opened = await client.attach(projectGrant, terminalId);
      activateOpened(opened, operation);
    } catch (reason) {
      if (!mountedRef.current || operationRef.current !== operation) return;
      setState({ ...idleState, error: describeTerminalError(reason), phase: "failed" });
    }
  }, [activateOpened, client, projectGrant]);

  const stop = useCallback(async () => {
    const transport = transportRef.current;
    if (!transport) return;
    setState((current) => ({ ...current, phase: "stopping" }));
    try {
      await transport.close();
    } catch {
      // The transport publishes the actionable disconnected state.
    }
  }, []);

  return {
    ...state,
    attach,
    listError,
    listLoading,
    reloadTerminals,
    start,
    stop,
    terminals,
  };
}

function statusToState(
  current: ShellTerminalState,
  identity: Pick<ShellTerminalState, "canonicalCwd" | "processId" | "runId" | "terminalId" | "transport">,
  snapshot: TerminalConnectionSnapshot,
): ShellTerminalState {
  return {
    ...current,
    ...identity,
    droppedThroughSequence: snapshot.droppedThroughSequence,
    error: snapshot.error,
    exitCode: snapshot.exit?.code ?? null,
    overflowed: snapshot.overflowed,
    phase: snapshot.state === "closing" ? "stopping" : snapshot.state,
    replayTruncated: snapshot.replayTruncated,
  };
}
