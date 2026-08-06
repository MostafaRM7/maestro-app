import { useCallback, useEffect, useRef, useState } from "react";
import {
  tauriFakeSessionClient,
  type FakeSessionClient,
  type SessionIndexEntry,
  type SessionTerminalConnection,
} from "../lib/fakeSession";
import type { TerminalTransport } from "../lib/terminal";
import {
  DaemonTerminalTransport,
  tauriTerminalCommandClient,
  type TerminalCommandClient,
  type TerminalConnectionSnapshot,
  type TerminalOpened,
} from "../lib/terminalClient";

export type FakeTuiPhase =
  | "idle"
  | "attaching"
  | "starting"
  | "running"
  | "stopping"
  | "stopped"
  | "exited"
  | "failed"
  | "disconnected";

export interface FakeTuiTransport extends TerminalTransport {
  detach(): void;
  startPolling(): void;
  subscribeStatus(listener: (snapshot: TerminalConnectionSnapshot) => void): () => void;
}

export type FakeTuiTransportFactory = (
  terminalClient: TerminalCommandClient,
  opened: TerminalOpened,
) => FakeTuiTransport;

export interface FakeTuiController {
  error: string | null;
  listError: string | null;
  listLoading: boolean;
  phase: FakeTuiPhase;
  sessions: readonly SessionIndexEntry[];
  sessionId: string | null;
  snapshot: TerminalConnectionSnapshot | null;
  transport: FakeTuiTransport | null;
  attach: (sessionId: string) => Promise<void>;
  reloadSessions: () => Promise<void>;
  start: (scenario: string) => Promise<void>;
  stop: () => Promise<void>;
}

const DEFAULT_TUI_COLUMNS = 100;
const DEFAULT_TUI_ROWS = 30;
const MAXIMUM_SESSION_INDEX_ENTRIES = 50;

export function useFakeTui(
  projectGrant: string,
  fakeClient: FakeSessionClient = tauriFakeSessionClient,
  terminalClient: TerminalCommandClient = tauriTerminalCommandClient,
  createTransport: FakeTuiTransportFactory = (client, opened) => new DaemonTerminalTransport(client, opened),
): FakeTuiController {
  const [phase, setPhase] = useState<FakeTuiPhase>("idle");
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [transport, setTransport] = useState<FakeTuiTransport | null>(null);
  const [snapshot, setSnapshot] = useState<TerminalConnectionSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sessions, setSessions] = useState<readonly SessionIndexEntry[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  const [listLoading, setListLoading] = useState(true);
  const mounted = useRef(false);
  const startInFlight = useRef(false);
  const stopInFlight = useRef(false);
  const sessionIdRef = useRef<string | null>(null);
  const transportRef = useRef<FakeTuiTransport | null>(null);
  const unsubscribeStatus = useRef<(() => void) | null>(null);

  const detachTransport = useCallback(() => {
    unsubscribeStatus.current?.();
    unsubscribeStatus.current = null;
    transportRef.current?.detach();
    transportRef.current = null;
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      // Detaching stops webview polling and input only. It deliberately never
      // calls terminal.close() or session_stop.
      detachTransport();
    };
  }, [detachTransport]);

  const reloadSessions = useCallback(async () => {
    setListLoading(true);
    setListError(null);
    try {
      const indexed = await fakeClient.listSessions(projectGrant, MAXIMUM_SESSION_INDEX_ENTRIES);
      if (mounted.current) setSessions(indexed);
    } catch {
      if (mounted.current) {
        setListError("Maestro could not read the persisted session index.");
      }
    } finally {
      if (mounted.current) setListLoading(false);
    }
  }, [fakeClient, projectGrant]);

  useEffect(() => {
    void reloadSessions();
  }, [reloadSessions]);

  const activateTerminal = useCallback((connection: SessionTerminalConnection) => {
    detachTransport();
    const nextTransport = createTransport(terminalClient, connection.terminal);
    transportRef.current = nextTransport;
    sessionIdRef.current = connection.sessionId;
    setSnapshot(null);
    unsubscribeStatus.current = nextTransport.subscribeStatus((nextSnapshot) => {
      if (!mounted.current) return;
      setSnapshot(nextSnapshot);
      if (!stopInFlight.current) setPhase(phaseFromSnapshot(nextSnapshot));
    });
    setSessionId(connection.sessionId);
    setTransport(nextTransport);
    setPhase("running");
    nextTransport.startPolling();
  }, [createTransport, detachTransport, terminalClient]);

  const start = useCallback(async (scenario: string) => {
    if (startInFlight.current || stopInFlight.current) return;
    startInFlight.current = true;
    setPhase("starting");
    setError(null);
    try {
      const started = await fakeClient.startTui(
        projectGrant,
        scenario,
        DEFAULT_TUI_COLUMNS,
        DEFAULT_TUI_ROWS,
      );
      if (!mounted.current) return;
      activateTerminal(started);
    } catch {
      if (mounted.current) {
        setPhase("failed");
        setError("Maestro could not start the deterministic fake TUI.");
      }
    } finally {
      startInFlight.current = false;
    }
  }, [activateTerminal, fakeClient, projectGrant]);

  const attach = useCallback(async (persistedSessionId: string) => {
    if (startInFlight.current || stopInFlight.current || transportRef.current) return;
    startInFlight.current = true;
    setPhase("attaching");
    setError(null);
    try {
      const attached = await fakeClient.attachTui(persistedSessionId);
      if (!mounted.current) return;
      activateTerminal(attached);
    } catch {
      if (mounted.current) {
        setPhase("failed");
        setError("Maestro could not reattach to the persisted fake TUI.");
      }
    } finally {
      startInFlight.current = false;
    }
  }, [activateTerminal, fakeClient]);

  const stop = useCallback(async () => {
    const activeSessionId = sessionIdRef.current;
    if (!activeSessionId || startInFlight.current || stopInFlight.current) return;
    stopInFlight.current = true;
    setPhase("stopping");
    setError(null);
    try {
      await fakeClient.stop(activeSessionId);
      if (!mounted.current) return;
      detachTransport();
      sessionIdRef.current = null;
      setSessionId(null);
      setTransport(null);
      setSnapshot(null);
      setPhase("stopped");
    } catch {
      if (mounted.current) {
        setPhase(snapshot?.state === "running" ? "running" : "failed");
        setError("Maestro could not stop the fake TUI session process group.");
      }
    } finally {
      stopInFlight.current = false;
    }
  }, [detachTransport, fakeClient, snapshot?.state]);

  return {
    error,
    listError,
    listLoading,
    phase,
    sessions,
    sessionId,
    snapshot,
    transport,
    attach,
    reloadSessions,
    start,
    stop,
  };
}

function phaseFromSnapshot(snapshot: TerminalConnectionSnapshot): FakeTuiPhase {
  switch (snapshot.state) {
    case "running":
      return "running";
    case "closing":
      return "stopping";
    case "exited":
    case "closed":
      return "exited";
    case "failed":
      return "failed";
    case "disconnected":
      return "disconnected";
  }
}
