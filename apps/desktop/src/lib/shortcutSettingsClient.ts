import { invoke } from "@tauri-apps/api/core";
import type { ShortcutBindings } from "./shortcuts";

export interface ShortcutSettingsClient {
  load: () => Promise<unknown>;
  save: (bindings: ShortcutBindings) => Promise<void>;
}

type InvokeCommand = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export function createTauriShortcutSettingsClient(
  invokeCommand: InvokeCommand = invoke,
): ShortcutSettingsClient {
  return {
    load() {
      return invokeCommand<unknown>("shortcut_settings_load");
    },
    save(bindings) {
      return invokeCommand<void>("shortcut_settings_save", { bindings });
    },
  };
}

export const tauriShortcutSettingsClient = createTauriShortcutSettingsClient();

