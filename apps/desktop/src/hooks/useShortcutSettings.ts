import { useCallback, useEffect, useState } from "react";
import {
  defaultShortcutBindings,
  normalizeShortcut,
  validateShortcutBindings,
  type ShortcutBindings,
  type ShortcutCommand,
} from "../lib/shortcuts";
import {
  tauriShortcutSettingsClient,
  type ShortcutSettingsClient,
} from "../lib/shortcutSettingsClient";

const shortcutCommands: readonly ShortcutCommand[] = [
  "openNewWindow",
  "openProject",
  "toggleBottomPanel",
  "toggleCommandPalette",
  "toggleInspector",
  "toggleSidebar",
];

export interface ShortcutSettingsState {
  bindings: ShortcutBindings;
  error: string | null;
  loading: boolean;
  save: (bindings: ShortcutBindings) => Promise<boolean>;
}

export function useShortcutSettings(
  client: ShortcutSettingsClient = tauriShortcutSettingsClient,
): ShortcutSettingsState {
  const [bindings, setBindings] = useState<ShortcutBindings>(defaultShortcutBindings());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    void client.load().then(
      (stored) => {
        if (!active) return;
        const validated = validateStoredBindings(stored);
        if (validated) setBindings(validated);
        setError(validated || stored === null
          ? null
          : "Saved shortcuts were invalid, so Maestro restored safe defaults.");
      },
      () => {
        if (active) setError("Maestro could not load encrypted shortcut settings. Safe defaults remain active.");
      },
    ).finally(() => {
      if (active) setLoading(false);
    });
    return () => {
      active = false;
    };
  }, [client]);

  const save = useCallback(async (next: ShortcutBindings) => {
    if (!validateStoredBindings(next)) {
      setError("Every shortcut must use Mod plus one letter or number, and shortcuts cannot conflict.");
      return false;
    }
    setError(null);
    try {
      await client.save(next);
      setBindings(next);
      return true;
    } catch {
      setError("Maestro could not save the encrypted shortcut settings.");
      return false;
    }
  }, [client]);

  return { bindings, error, loading, save };
}

function validateStoredBindings(value: unknown): ShortcutBindings | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const candidate = { ...defaultShortcutBindings() } as Record<ShortcutCommand, string>;
  for (const command of shortcutCommands) {
    const shortcut = record[command];
    if (typeof shortcut !== "string") return null;
    const normalized = normalizeShortcut(shortcut);
    if (!normalized) return null;
    candidate[command] = normalized;
  }
  return validateShortcutBindings(candidate).length === 0 ? candidate : null;
}

