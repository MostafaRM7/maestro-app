import { invoke } from "@tauri-apps/api/core";

export interface WindowLayoutClient {
  load: (projectGrant: string, windowKey: string) => Promise<string | null>;
  save: (projectGrant: string, windowKey: string, layoutJson: string) => Promise<void>;
}

type InvokeCommand = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export function createTauriWindowLayoutClient(
  invokeCommand: InvokeCommand = invoke,
): WindowLayoutClient {
  return {
    load(projectGrant, windowKey) {
      return invokeCommand<string | null>("project_window_layout_load", {
        projectGrant,
        windowKey,
      });
    },
    save(projectGrant, windowKey, layoutJson) {
      return invokeCommand<void>("project_window_layout_save", {
        layoutJson,
        projectGrant,
        windowKey,
      });
    },
  };
}

export const tauriWindowLayoutClient = createTauriWindowLayoutClient();
