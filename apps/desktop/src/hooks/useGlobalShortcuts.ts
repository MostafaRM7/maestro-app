import { useEffect } from "react";
import {
  defaultShortcutBindings,
  isShortcutOwnedTarget,
  shortcutMatches,
  type ShortcutBindings,
  type ShortcutCommand,
} from "../lib/shortcuts";

export interface ShortcutActions {
  cycleFocus: (backwards: boolean) => void;
  openProject: () => void;
  openNewWindow: () => void;
  toggleBottomPanel: () => void;
  toggleCommandPalette: () => void;
  toggleInspector: () => void;
  toggleSidebar: () => void;
}

interface ShortcutOptions {
  bindings?: ShortcutBindings;
  enabled: boolean;
  platform: string;
}

export function useGlobalShortcuts(actions: ShortcutActions, options: ShortcutOptions): void {
  useEffect(() => {
    if (!options.enabled) return;

    const onKeyDown = (event: KeyboardEvent) => {
      if (isShortcutOwnedTarget(event)) return;
      if (event.key === "F6") {
        event.preventDefault();
        actions.cycleFocus(event.shiftKey);
        return;
      }

      const bindings = options.bindings ?? defaultShortcutBindings();
      const command = (Object.keys(bindings) as ShortcutCommand[])
        .find((candidate) => shortcutMatches(event, bindings[candidate], options.platform));
      if (!command) return;
      event.preventDefault();
      actions[command]();
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [actions, options.bindings, options.enabled, options.platform]);
}
