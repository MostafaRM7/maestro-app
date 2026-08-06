import { useCallback, useEffect, useState } from "react";
import { ProjectWindow } from "./components/ProjectWindow";
import {
  ConnectingSurface,
  DaemonUnavailableSurface,
  StorageUnavailableSurface,
  StorageUnlockSurface,
} from "./components/StartupSurfaces";
import { WelcomeWindow } from "./components/WelcomeWindow";
import { useAppearance } from "./hooks/useAppearance";
import {
  tauriDesktopHostClient,
  type DesktopHostClient,
  type ProjectSelection,
  type RecentProject,
  type SystemSnapshot,
} from "./lib/system";
import { tauriTerminalCommandClient, type TerminalCommandClient } from "./lib/terminalClient";
import type { WindowLayoutClient } from "./lib/windowLayoutClient";
import { tauriNativeWindowClient, type NativeWindowClient } from "./lib/windowClient";

type BootstrapState =
  | { kind: "connecting" }
  | { kind: "failed"; error: string }
  | { kind: "resolved"; snapshot: SystemSnapshot };

export interface AppProps {
  hostClient?: DesktopHostClient;
  initialProjectName?: string;
  layoutClient?: WindowLayoutClient;
  terminalClient?: TerminalCommandClient;
  windowClient?: NativeWindowClient;
}

export function App({
  hostClient = tauriDesktopHostClient,
  initialProjectName,
  layoutClient,
  terminalClient = tauriTerminalCommandClient,
  windowClient = tauriNativeWindowClient,
}: AppProps) {
  const appearance = useAppearance();
  const [bootstrap, setBootstrap] = useState<BootstrapState>({ kind: "connecting" });
  const [project, setProject] = useState<ProjectSelection | null>(() => initialProjectName
    ? { id: `test:${initialProjectName}`, name: initialProjectName, roots: [] }
    : null);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [projectOpenError, setProjectOpenError] = useState<string | null>(null);
  const [projectPickerOpen, setProjectPickerOpen] = useState(false);
  const [recentProjects, setRecentProjects] = useState<RecentProject[]>([]);
  const [recentProjectsLoading, setRecentProjectsLoading] = useState(false);
  const [openingRecentProjectId, setOpeningRecentProjectId] = useState<string | null>(null);
  const [favoriteProjectId, setFavoriteProjectId] = useState<string | null>(null);

  const connect = useCallback(() => {
    setBootstrap({ kind: "connecting" });
    void hostClient.read().then(
      (snapshot) => setBootstrap({ kind: "resolved", snapshot }),
      (reason: unknown) => setBootstrap({
        kind: "failed",
        error: reason instanceof Error ? reason.message : "The desktop host did not respond.",
      }),
    );
  }, [hostClient]);

  const openProject = useCallback(async () => {
    if (projectPickerOpen || openingRecentProjectId !== null) return;
    setProjectError(null);
    setProjectOpenError(null);
    setProjectPickerOpen(true);
    try {
      const selection = await hostClient.openProject();
      if (selection) setProject(selection);
    } catch {
      const message = "Maestro could not open the selected project folder. Choose another folder and try again.";
      setProjectError(message);
      setProjectOpenError(message);
    } finally {
      setProjectPickerOpen(false);
    }
  }, [hostClient, openingRecentProjectId, projectPickerOpen]);

  const openRecentProject = useCallback(async (projectId: string) => {
    if (projectPickerOpen || openingRecentProjectId !== null) return;
    setProjectError(null);
    setOpeningRecentProjectId(projectId);
    try {
      setProject(await hostClient.openRecentProject(projectId));
    } catch {
      setProjectError("Maestro could not safely reopen this project. Confirm that its saved folders still exist and try again.");
    } finally {
      setOpeningRecentProjectId(null);
    }
  }, [hostClient, openingRecentProjectId, projectPickerOpen]);

  const setProjectFavorite = useCallback(async (projectId: string, favorite: boolean) => {
    if (favoriteProjectId !== null) return;
    setProjectError(null);
    setFavoriteProjectId(projectId);
    try {
      const updated = await hostClient.setProjectFavorite(projectId, favorite);
      setRecentProjects((projects) => projects.map((projectItem) => projectItem.projectId === projectId
        ? { ...projectItem, favorite: updated }
        : projectItem));
    } catch {
      setProjectError("Maestro could not update this favorite. Try again.");
    } finally {
      setFavoriteProjectId(null);
    }
  }, [favoriteProjectId, hostClient]);

  const unlockStorage = useCallback(async (passphrase: string) => {
    const snapshot = await hostClient.unlockStorage(passphrase);
    setBootstrap({ kind: "resolved", snapshot });
  }, [hostClient]);

  const openNewWindow = useCallback(() => {
    void windowClient.openNewWindow().catch(() => {
      setProjectError("Maestro could not open another native window.");
    });
  }, [windowClient]);

  useEffect(() => connect(), [connect]);

  useEffect(() => {
    if (bootstrap.kind === "resolved") {
      document.documentElement.dataset.platform = bootstrap.snapshot.platform;
    }
  }, [bootstrap]);

  useEffect(() => {
    if (
      bootstrap.kind !== "resolved"
      || bootstrap.snapshot.daemon.status !== "connected"
      || bootstrap.snapshot.daemon.storageStatus !== "ready"
    ) {
      setRecentProjects([]);
      setRecentProjectsLoading(false);
      return;
    }
    let active = true;
    setRecentProjectsLoading(true);
    void hostClient.listRecentProjects(20).then(
      (projects) => {
        if (active) setRecentProjects(projects);
      },
      () => {
        if (active) setProjectError("Maestro could not load recent projects.");
      },
    ).finally(() => {
      if (active) setRecentProjectsLoading(false);
    });
    return () => {
      active = false;
    };
  }, [bootstrap, hostClient]);

  if (bootstrap.kind === "connecting") return <ConnectingSurface />;
  if (bootstrap.kind === "failed") return <DaemonUnavailableSurface error={bootstrap.error} onRetry={connect} />;
  if (bootstrap.snapshot.daemon.status === "connected") {
    if (bootstrap.snapshot.daemon.storageStatus === "passphraseCreateRequired") {
      return <StorageUnlockSurface mode="create" onUnlock={unlockStorage} />;
    }
    if (bootstrap.snapshot.daemon.storageStatus === "passphraseUnlockRequired") {
      return <StorageUnlockSurface mode="unlock" onUnlock={unlockStorage} />;
    }
    if (bootstrap.snapshot.daemon.storageStatus === "unavailable") {
      return <StorageUnavailableSurface onRetry={connect} snapshot={bootstrap.snapshot} />;
    }
  }
  if (project) {
    return (
      <ProjectWindow
        key={`${project.id}:${bootstrap.snapshot.windowLabel}`}
        appearance={appearance}
        daemon={bootstrap.snapshot.daemon}
        layoutClient={layoutClient}
        onOpenProject={() => void openProject()}
        onOpenNewWindow={openNewWindow}
        openingProject={projectPickerOpen}
        platform={bootstrap.snapshot.platform}
        project={project}
        projectError={projectOpenError}
        terminalClient={terminalClient}
        windowId={bootstrap.snapshot.windowLabel}
      />
    );
  }
  return (
    <WelcomeWindow
      daemon={bootstrap.snapshot.daemon}
      error={projectError}
      favoriteProjectId={favoriteProjectId}
      onOpenRecentProject={(projectId) => void openRecentProject(projectId)}
      onOpenProject={() => void openProject()}
      onOpenNewWindow={openNewWindow}
      onSetFavorite={(projectId, favorite) => void setProjectFavorite(projectId, favorite)}
      openingProject={projectPickerOpen || openingRecentProjectId !== null}
      openingRecentProjectId={openingRecentProjectId}
      recentProjects={recentProjects}
      recentProjectsLoading={recentProjectsLoading}
      snapshot={bootstrap.snapshot}
    />
  );
}
