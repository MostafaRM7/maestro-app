import { useCallback, useEffect, useRef, useState } from "react";
import {
  tauriWindowLayoutClient,
  type WindowLayoutClient,
} from "../lib/windowLayoutClient";

export type Activity = "sessions" | "files" | "search" | "git";
export type BottomSurface = "events" | "raw" | "agent" | "shell";
export type WorkspaceSurface = "conversation" | "plan";

export interface WindowLayoutState {
  activity: Activity;
  bottomHeight: number;
  bottomPanelOpen: boolean;
  bottomSurface: BottomSurface;
  inspectorOpen: boolean;
  inspectorWidth: number;
  sidebarOpen: boolean;
  sidebarWidth: number;
  workspaceSurface: WorkspaceSurface;
}

const ACTIVITIES: readonly Activity[] = ["sessions", "files", "search", "git"];
const BOTTOM_SURFACES: readonly BottomSurface[] = ["events", "raw", "agent", "shell"];
const WORKSPACE_SURFACES: readonly WorkspaceSurface[] = ["conversation", "plan"];
const SAVE_DEBOUNCE_MILLISECONDS = 300;

export const DEFAULT_WINDOW_LAYOUT: WindowLayoutState = {
  activity: "sessions",
  bottomHeight: 220,
  bottomPanelOpen: true,
  bottomSurface: "events",
  inspectorOpen: true,
  inspectorWidth: 300,
  sidebarOpen: true,
  sidebarWidth: 260,
  workspaceSurface: "conversation",
};

function enumValue<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return typeof value === "string" && allowed.includes(value as T) ? value as T : fallback;
}

function boundedNumber(value: unknown, minimum: number, maximum: number, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(maximum, Math.max(minimum, value))
    : fallback;
}

export function parseWindowLayout(value: string | null): WindowLayoutState {
  if (!value) return DEFAULT_WINDOW_LAYOUT;
  try {
    const candidate = JSON.parse(value) as Record<string, unknown>;
    return {
      activity: enumValue(candidate.activity, ACTIVITIES, DEFAULT_WINDOW_LAYOUT.activity),
      bottomHeight: boundedNumber(candidate.bottomHeight, 120, 600, DEFAULT_WINDOW_LAYOUT.bottomHeight),
      bottomPanelOpen: typeof candidate.bottomPanelOpen === "boolean" ? candidate.bottomPanelOpen : true,
      bottomSurface: enumValue(candidate.bottomSurface, BOTTOM_SURFACES, DEFAULT_WINDOW_LAYOUT.bottomSurface),
      inspectorOpen: typeof candidate.inspectorOpen === "boolean" ? candidate.inspectorOpen : true,
      inspectorWidth: boundedNumber(candidate.inspectorWidth, 240, 440, DEFAULT_WINDOW_LAYOUT.inspectorWidth),
      sidebarOpen: typeof candidate.sidebarOpen === "boolean" ? candidate.sidebarOpen : true,
      sidebarWidth: boundedNumber(candidate.sidebarWidth, 200, 420, DEFAULT_WINDOW_LAYOUT.sidebarWidth),
      workspaceSurface: enumValue(candidate.workspaceSurface, WORKSPACE_SURFACES, DEFAULT_WINDOW_LAYOUT.workspaceSurface),
    };
  } catch {
    return DEFAULT_WINDOW_LAYOUT;
  }
}

export function windowLayoutStorageKey(projectId: string, windowId: string): string {
  return `${encodeURIComponent(projectId)}:${encodeURIComponent(windowId)}`;
}

export function useWindowLayout(
  projectId: string,
  windowId: string,
  client: WindowLayoutClient = tauriWindowLayoutClient,
) {
  const [layout, setLayout] = useState<WindowLayoutState>(DEFAULT_WINDOW_LAYOUT);
  const [hydrated, setHydrated] = useState(false);
  const changedBeforeLoad = useRef(false);
  const hydratedRef = useRef(false);
  const lastPersisted = useRef<string | null>(null);
  const pendingLayout = useRef<string | null>(null);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let active = true;
    changedBeforeLoad.current = false;
    hydratedRef.current = false;
    lastPersisted.current = null;
    pendingLayout.current = null;
    setLayout(DEFAULT_WINDOW_LAYOUT);
    setHydrated(false);

    void client.load(projectId, windowId).then((stored) => {
      if (!active) return;
      const restored = parseWindowLayout(stored);
      lastPersisted.current = JSON.stringify(restored);
      if (!changedBeforeLoad.current) setLayout(restored);
      hydratedRef.current = true;
      setHydrated(true);
    }).catch(() => {
      // Keep safe defaults without overwriting persisted state after a transient load failure.
    });

    return () => {
      active = false;
      if (saveTimer.current !== null) clearTimeout(saveTimer.current);
      saveTimer.current = null;
      const pending = pendingLayout.current;
      if (hydratedRef.current && pending !== null && pending !== lastPersisted.current) {
        void client.save(projectId, windowId, pending).catch(() => undefined);
      }
    };
  }, [client, projectId, windowId]);

  const update = useCallback((patch: Partial<WindowLayoutState>) => {
    changedBeforeLoad.current = true;
    setLayout((current) => ({ ...current, ...patch }));
  }, []);

  useEffect(() => {
    if (!hydrated) return;
    const serialized = JSON.stringify(layout);
    pendingLayout.current = serialized;
    if (serialized === lastPersisted.current) return;
    if (saveTimer.current !== null) clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(() => {
      saveTimer.current = null;
      void client.save(projectId, windowId, serialized).then(() => {
        lastPersisted.current = serialized;
      }).catch(() => undefined);
    }, SAVE_DEBOUNCE_MILLISECONDS);
  }, [client, hydrated, layout, projectId, windowId]);

  return { hydrated, layout, update };
}
