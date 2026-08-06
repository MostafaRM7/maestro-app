import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  isSessionSettled,
  mergeSessionEvents,
  tauriFakeSessionClient,
  type EventEnvelope,
  type FakeSessionClient,
  type PermissionDecision,
  type SessionRunStarted,
  type SessionIndexEntry,
  type SessionRawBatch,
  type SessionSnapshot,
  type SessionState,
} from "../lib/fakeSession";

export interface PendingSessionRequest {
  eventId: string;
  payload: Record<string, unknown>;
  requestId: string;
  runId: string;
}

export interface FakeSessionController {
  attach: (sessionId: string) => Promise<void>;
  error: string | null;
  events: readonly EventEnvelope[];
  launching: boolean;
  listError: string | null;
  listLoading: boolean;
  pendingInput: PendingSessionRequest | null;
  pendingPermission: PendingSessionRequest | null;
  replayGap: boolean;
  rawCaptureEnabled: boolean;
  rawCaptureError: string | null;
  rawProtocol: SessionRawBatch | null;
  resolvedRequestIds: ReadonlySet<string>;
  reloadSessions: () => Promise<void>;
  run: SessionRunStarted | null;
  snapshot: SessionSnapshot | null;
  sessions: readonly SessionIndexEntry[];
  state: SessionState | null;
  stopping: boolean;
  setRawCaptureEnabled: (enabled: boolean) => void;
  start: (scenario: string) => Promise<void>;
  resume: () => Promise<void>;
  stop: () => Promise<void>;
  respondPermission: (
    request: PendingSessionRequest,
    decision: PermissionDecision,
  ) => Promise<boolean>;
  respondUserInput: (
    request: PendingSessionRequest,
    value: unknown,
  ) => Promise<boolean>;
  sendDemoGuiAction: () => Promise<void>;
}

export function useFakeSession(
  projectGrant: string,
  client: FakeSessionClient = tauriFakeSessionClient,
  rawDisplayActive = false,
): FakeSessionController {
  const [run, setRun] = useState<SessionRunStarted | null>(null);
  const [snapshot, setSnapshot] = useState<SessionSnapshot | null>(null);
  const [events, setEvents] = useState<readonly EventEnvelope[]>([]);
  const [state, setState] = useState<SessionState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [launching, setLaunching] = useState(false);
  const [sessions, setSessions] = useState<readonly SessionIndexEntry[]>([]);
  const [listLoading, setListLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [replayGap, setReplayGap] = useState(false);
  const [rawCaptureEnabled, setRawCaptureEnabled] = useState(false);
  const [rawCaptureError, setRawCaptureError] = useState<string | null>(null);
  const [rawProtocol, setRawProtocol] = useState<SessionRawBatch | null>(null);
  const [resolvedRequestIds, setResolvedRequestIds] = useState<ReadonlySet<string>>(new Set());
  const resolvedRequestIdsRef = useRef(new Set<string>());
  const cursor = useRef(0);
  const rawCursor = useRef(0);

  const reloadSessions = useCallback(async () => {
    setListLoading(true);
    setListError(null);
    try {
      setSessions(await client.listSessions(projectGrant, 50));
    } catch {
      setListError("Maestro could not restore the persisted structured-session index.");
    } finally {
      setListLoading(false);
    }
  }, [client, projectGrant]);

  useEffect(() => {
    void reloadSessions();
  }, [reloadSessions]);

  useEffect(() => {
    if (rawDisplayActive) return;
    rawCursor.current = 0;
    setRawProtocol(null);
    setRawCaptureError(null);
  }, [rawDisplayActive, run?.runId]);

  useEffect(() => {
    if (!run) return;
    const controller = new AbortController();
    let active = true;
    let subscribed = false;
    const sessionId = run.sessionId;
    const runId = run.runId;

    const readRawPages = async () => {
      if (!rawCaptureEnabled || !rawDisplayActive) return;
      for (let page = 0; page < 8; page += 1) {
        const afterOffset = rawCursor.current;
        const batch = await client.readRaw(
          sessionId,
          runId,
          afterOffset,
          256 * 1024,
          controller.signal,
        );
        if (!active) return;
        if (
          batch.sessionId !== sessionId
          || batch.runId !== runId
          || batch.nextOffset < afterOffset
          || batch.nextOffset - afterOffset !== batch.data.length
        ) {
          throw new Error("Invalid raw protocol cursor response");
        }
        setRawProtocol((current) => ({
          ...batch,
          data: current && current.runId === runId
            ? [...current.data, ...batch.data]
            : [...batch.data],
        }));
        rawCursor.current = batch.nextOffset;
        if (batch.nextOffset >= batch.capturedBytes) break;
      }
    };

    void (async () => {
      try {
        await client.subscribe(sessionId, cursor.current);
        subscribed = true;
        if (rawCaptureEnabled && rawDisplayActive) {
          try {
            await readRawPages();
            setRawCaptureError(null);
          } catch (cause) {
            if (!isAbortError(cause)) {
              setRawCaptureError("The sensitive raw capture could not be read. Normalized events remain available.");
            }
          }
        }
        while (active && !controller.signal.aborted) {
          const batch = await client.readEvents(sessionId, cursor.current, controller.signal);
          if (!active) break;
          cursor.current = Math.max(cursor.current, batch.nextSequence);
          setEvents((current) => mergeSessionEvents(current, batch.events));
          setState(batch.state);
          setReplayGap((current) => current || batch.replayGap !== null);
          if (rawCaptureEnabled && rawDisplayActive) {
            try {
              await readRawPages();
              setRawCaptureError(null);
            } catch (cause) {
              if (!isAbortError(cause)) {
                setRawCaptureError("The sensitive raw capture could not be read. Normalized events remain available.");
              }
            }
          }
          if (isSessionSettled(batch.state)) {
            const finalSnapshot = await client.snapshot(sessionId);
            if (active) {
              setSnapshot(finalSnapshot);
              setState(finalSnapshot.state);
              setStopping(false);
            }
            break;
          }
        }
      } catch (cause) {
        if (active && !isAbortError(cause)) {
          setError("The fake-session event stream disconnected. The local session may still be running.");
        }
      } finally {
        if (subscribed) void client.unsubscribe(sessionId).catch(() => undefined);
      }
    })();

    return () => {
      active = false;
      controller.abort();
    };
  }, [client, rawCaptureEnabled, rawDisplayActive, run]);

  const start = useCallback(async (scenario: string) => {
    if (launching) return;
    setLaunching(true);
    setError(null);
    setReplayGap(false);
    setRawCaptureError(null);
    setRawProtocol(null);
    resolvedRequestIdsRef.current = new Set();
    setResolvedRequestIds(new Set());
    setEvents([]);
    setSnapshot(null);
    setState("starting");
    cursor.current = 0;
    rawCursor.current = 0;
    try {
      const started = await client.start(projectGrant, scenario, null, null, rawCaptureEnabled);
      setRun(started);
    } catch {
      setState(null);
      setError("Maestro could not start the deterministic fake session.");
    } finally {
      setLaunching(false);
    }
  }, [client, launching, projectGrant, rawCaptureEnabled]);

  const attach = useCallback(async (sessionId: string) => {
    if (launching) return;
    setLaunching(true);
    setError(null);
    setReplayGap(false);
    setRawCaptureError(null);
    setRawProtocol(null);
    resolvedRequestIdsRef.current = new Set();
    setResolvedRequestIds(new Set());
    setEvents([]);
    setSnapshot(null);
    setState("starting");
    cursor.current = 0;
    rawCursor.current = 0;
    try {
      const attached = await client.attach(projectGrant, sessionId);
      setRun(attached);
    } catch {
      setState(null);
      setError("Maestro could not attach to the active structured fake session.");
    } finally {
      setLaunching(false);
    }
  }, [client, launching, projectGrant]);

  const resume = useCallback(async () => {
    if (!run || launching) return;
    setLaunching(true);
    setError(null);
    resolvedRequestIdsRef.current = new Set();
    setResolvedRequestIds(new Set());
    setState("starting");
    setRawCaptureError(null);
    setRawProtocol(null);
    rawCursor.current = 0;
    try {
      const resumed = await client.resume(
        projectGrant,
        run.sessionId,
        "structured/resume",
        snapshot?.binding ?? null,
        rawCaptureEnabled,
      );
      setRun(resumed);
      setSnapshot(null);
    } catch {
      setError("Maestro could not resume this deterministic fake session.");
    } finally {
      setLaunching(false);
    }
  }, [client, launching, projectGrant, rawCaptureEnabled, run, snapshot?.binding]);

  const stop = useCallback(async () => {
    if (!run || stopping || state === null || isSessionSettled(state)) return;
    setStopping(true);
    setError(null);
    try {
      await client.stop(run.sessionId);
      setState("interrupting");
    } catch {
      setError("Maestro could not stop the fake session process group.");
      setStopping(false);
    }
  }, [client, run, state, stopping]);

  const respondPermission = useCallback(async (
    request: PendingSessionRequest,
    decision: PermissionDecision,
  ): Promise<boolean> => {
    if (!run || resolvedRequestIdsRef.current.has(request.requestId)) return false;
    resolvedRequestIdsRef.current.add(request.requestId);
    setResolvedRequestIds((current) => new Set(current).add(request.requestId));
    setError(null);
    try {
      await client.respondPermission(
        run.sessionId,
        request.runId,
        request.requestId,
        decision,
      );
      return true;
    } catch (reason) {
      if (isRetrySafeDeliveryError(reason)) {
        releaseResolvedRequest(request.requestId, resolvedRequestIdsRef, setResolvedRequestIds);
        setError("The permission response was not delivered. Retry it while the request remains active.");
      } else {
        setError("Permission delivery may have reached the CLI. Retry is disabled until the session reports the result.");
      }
      return false;
    }
  }, [client, run]);

  const respondUserInput = useCallback(async (
    request: PendingSessionRequest,
    value: unknown,
  ): Promise<boolean> => {
    if (!run || resolvedRequestIdsRef.current.has(request.requestId)) return false;
    resolvedRequestIdsRef.current.add(request.requestId);
    setResolvedRequestIds((current) => new Set(current).add(request.requestId));
    setError(null);
    try {
      await client.respondUserInput(run.sessionId, request.runId, request.requestId, value);
      return true;
    } catch (reason) {
      if (isRetrySafeDeliveryError(reason)) {
        releaseResolvedRequest(request.requestId, resolvedRequestIdsRef, setResolvedRequestIds);
        setError("The user-input response was not delivered. Retry it while the request remains active.");
      } else {
        setError("User-input delivery may have reached the CLI. Retry is disabled until the session reports the result.");
      }
      return false;
    }
  }, [client, run]);

  const sendDemoGuiAction = useCallback(async () => {
    if (!run || state === null || isSessionSettled(state)) return;
    setError(null);
    try {
      await client.sendGuiAction(run.sessionId, run.runId, "session.inspect", {
        origin: "maestro.fake_harness",
      });
    } catch {
      setError("The fake CLI did not accept the annotated GUI action.");
    }
  }, [client, run, state]);

  const pendingPermission = useMemo(
    () => findPendingRequest(events, "permission_request", ["gui_permission_response", "permission_result"]),
    [events],
  );
  const pendingInput = useMemo(
    () => findPendingRequest(events, "user_input_request", ["gui_user_input_response", "user_input_result"]),
    [events],
  );

  useEffect(() => {
    const pendingIds = new Set(
      [pendingPermission?.requestId, pendingInput?.requestId]
        .filter((requestId): requestId is string => requestId !== undefined),
    );
    resolvedRequestIdsRef.current = new Set(
      [...resolvedRequestIdsRef.current].filter((requestId) => pendingIds.has(requestId)),
    );
    setResolvedRequestIds((current) => {
      const next = new Set([...current].filter((requestId) => pendingIds.has(requestId)));
      return setsEqual(current, next) ? current : next;
    });
  }, [pendingInput?.requestId, pendingPermission?.requestId]);

  return {
    attach,
    error,
    events,
    launching,
    listError,
    listLoading,
    pendingInput,
    pendingPermission,
    replayGap,
    rawCaptureEnabled,
    rawCaptureError,
    rawProtocol,
    resolvedRequestIds,
    reloadSessions,
    run,
    snapshot,
    sessions,
    state,
    stopping,
    setRawCaptureEnabled,
    start,
    resume,
    stop,
    respondPermission,
    respondUserInput,
    sendDemoGuiAction,
  };
}

function isRetrySafeDeliveryError(reason: unknown): boolean {
  if (!reason || typeof reason !== "object") return false;
  const details = (reason as { details?: unknown }).details;
  return Boolean(details)
    && typeof details === "object"
    && (details as { retry_safe?: unknown }).retry_safe === true;
}

function releaseResolvedRequest(
  requestId: string,
  resolvedRequestIdsRef: { current: Set<string> },
  setResolvedRequestIds: (
    update: (current: ReadonlySet<string>) => ReadonlySet<string>
  ) => void,
) {
  resolvedRequestIdsRef.current.delete(requestId);
  setResolvedRequestIds((current) => {
    const next = new Set(current);
    next.delete(requestId);
    return next;
  });
}

function findPendingRequest(
  events: readonly EventEnvelope[],
  requestKind: string,
  responseKinds: readonly string[],
): PendingSessionRequest | null {
  const resolved = new Set<string>();
  for (const envelope of events) {
    if (![...responseKinds, "request_expired"].includes(envelope.event.kind)) continue;
    const requestId = payloadString(envelope.event.payload, "request_id");
    if (requestId) resolved.add(requestId);
  }
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const envelope = events[index];
    if (envelope.event.kind !== requestKind || envelope.run_id === null) continue;
    const requestId = payloadString(envelope.event.payload, "request_id");
    if (!requestId || resolved.has(requestId)) continue;
    return {
      eventId: envelope.event_id,
      payload: objectPayload(envelope.event.payload),
      requestId,
      runId: envelope.run_id,
    };
  }
  return null;
}

function objectPayload(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function payloadString(value: unknown, key: string): string | null {
  const field = objectPayload(value)[key];
  return typeof field === "string" ? field : null;
}

function isAbortError(cause: unknown): boolean {
  return cause instanceof Error && cause.name === "AbortError";
}

function setsEqual(left: ReadonlySet<string>, right: ReadonlySet<string>): boolean {
  return left.size === right.size && [...left].every((value) => right.has(value));
}
