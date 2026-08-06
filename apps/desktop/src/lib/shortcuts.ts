export type ShortcutCommand =
  | "openNewWindow"
  | "openProject"
  | "toggleBottomPanel"
  | "toggleCommandPalette"
  | "toggleInspector"
  | "toggleSidebar";

export type ShortcutBindings = Readonly<Record<ShortcutCommand, string>>;

export interface ShortcutConflict {
  commands: readonly ShortcutCommand[];
  shortcut: string;
}

const commands: readonly ShortcutCommand[] = [
  "openNewWindow",
  "openProject",
  "toggleBottomPanel",
  "toggleCommandPalette",
  "toggleInspector",
  "toggleSidebar",
];

const defaultBindings: ShortcutBindings = {
  openNewWindow: "Mod+Shift+N",
  openProject: "Mod+O",
  toggleBottomPanel: "Mod+J",
  toggleCommandPalette: "Mod+Shift+P",
  toggleInspector: "Mod+Shift+B",
  toggleSidebar: "Mod+B",
};

export function defaultShortcutBindings(): ShortcutBindings {
  return defaultBindings;
}

export function normalizeShortcut(shortcut: string): string | null {
  const parts = shortcut
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean);
  if (parts.length === 0) return null;

  const key = parts.at(-1)?.toUpperCase();
  if (!key || key.length !== 1 || !/[A-Z0-9]/u.test(key)) return null;
  const modifiers = new Set(parts.slice(0, -1).map((part) => part.toLowerCase()));
  if ([...modifiers].some((modifier) => modifier !== "mod" && modifier !== "shift")) return null;
  if (!modifiers.has("mod")) return null;
  return `Mod+${modifiers.has("shift") ? "Shift+" : ""}${key}`;
}

export function validateShortcutBindings(bindings: ShortcutBindings): readonly ShortcutConflict[] {
  const byShortcut = new Map<string, ShortcutCommand[]>();
  for (const command of commands) {
    const normalized = normalizeShortcut(bindings[command]);
    if (!normalized) continue;
    const owners = byShortcut.get(normalized) ?? [];
    owners.push(command);
    byShortcut.set(normalized, owners);
  }
  return [...byShortcut.entries()]
    .filter(([, owners]) => owners.length > 1)
    .map(([shortcut, owners]) => ({ commands: owners, shortcut }));
}

export function shortcutMatches(
  event: KeyboardEvent,
  shortcut: string,
  platform: string,
): boolean {
  const normalized = normalizeShortcut(shortcut);
  if (!normalized) return false;
  const parts = normalized.split("+");
  const expectedKey = parts.at(-1)?.toLowerCase();
  const expectedShift = parts.includes("Shift");
  const mod = platform === "macos"
    ? event.metaKey && !event.ctrlKey
    : event.ctrlKey && !event.metaKey;
  return mod
    && event.altKey === false
    && event.shiftKey === expectedShift
    && event.key.toLowerCase() === expectedKey;
}

export function shortcutLabel(shortcut: string, platform: string): string {
  const normalized = normalizeShortcut(shortcut);
  if (!normalized) return "Unassigned";
  return platform === "macos"
    ? normalized.replace("Mod+", "⌘").replace("Shift+", "⇧")
    : normalized.replace("Mod+", "Ctrl+");
}

export function isShortcutOwnedTarget(event: KeyboardEvent): boolean {
  const path = typeof event.composedPath === "function" ? event.composedPath() : [];
  const nodes = path.length > 0 ? path : [event.target];
  return nodes.some((candidate) => {
    if (!(candidate instanceof HTMLElement)) return false;
    if (candidate.closest("[data-terminal-input='true']")) return true;
    if (candidate.isContentEditable) return true;
    return candidate.matches("input, textarea, select, [role='textbox']");
  });
}

