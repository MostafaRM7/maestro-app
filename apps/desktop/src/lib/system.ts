import { invoke } from "@tauri-apps/api/core";

export type DaemonStatus = "connected" | "connecting" | "notConnected" | "failed";
export type DaemonStorageStatus =
  | "ready"
  | "passphraseCreateRequired"
  | "passphraseUnlockRequired"
  | "unavailable";

export interface DaemonSnapshot {
  status: DaemonStatus;
  detail: string;
  storageStatus: DaemonStorageStatus;
  storageSchemaVersion: number | null;
}

export interface SystemSnapshot {
  appVersion: string;
  platform: string;
  architecture: string;
  windowLabel: string;
  daemon: DaemonSnapshot;
}

export interface ProjectSelection {
  id: string;
  name: string;
  roots: string[];
}

export interface RecentProject {
  projectId: string;
  displayName: string;
  canonicalRoots: string[];
  favorite: boolean;
  lastOpenedAt: string;
}

export interface DesktopHostClient {
  listRecentProjects: (maximumProjects: number) => Promise<RecentProject[]>;
  openProject: () => Promise<ProjectSelection | null>;
  openRecentProject: (projectId: string) => Promise<ProjectSelection>;
  read: () => Promise<SystemSnapshot>;
  setProjectFavorite: (projectId: string, favorite: boolean) => Promise<boolean>;
  unlockStorage: (passphrase: string) => Promise<SystemSnapshot>;
}

type InvokeCommand = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export function createTauriDesktopHostClient(invokeCommand: InvokeCommand = invoke): DesktopHostClient {
  return {
    listRecentProjects(maximumProjects) {
      return invokeCommand<RecentProject[]>("project_recent_list", { maximumProjects });
    },
    openProject() {
      return invokeCommand<ProjectSelection | null>("open_project_folder");
    },
    openRecentProject(projectId) {
      return invokeCommand<ProjectSelection>("open_recent_project", { projectId });
    },
    read() {
      return invokeCommand<SystemSnapshot>("system_snapshot");
    },
    setProjectFavorite(projectId, favorite) {
      return invokeCommand<boolean>("project_set_favorite", { favorite, projectId });
    },
    unlockStorage(passphrase) {
      return invokeCommand<SystemSnapshot>("storage_unlock", { passphrase });
    },
  };
}

export const tauriDesktopHostClient = createTauriDesktopHostClient();

export function describeHost(snapshot: SystemSnapshot): string {
  const platform = snapshot.platform === "macos" ? "macOS" : snapshot.platform;
  return `${platform} · ${snapshot.architecture} · Maestro ${snapshot.appVersion}`;
}
