import { useEffect, useMemo, useRef, useState } from "react";
import {
  defaultShortcutBindings,
  normalizeShortcut,
  validateShortcutBindings,
  type ShortcutBindings,
  type ShortcutCommand,
} from "../lib/shortcuts";

const fields: ReadonlyArray<{ command: ShortcutCommand; label: string }> = [
  { command: "openProject", label: "Open project" },
  { command: "openNewWindow", label: "Open new window" },
  { command: "toggleSidebar", label: "Toggle primary sidebar" },
  { command: "toggleInspector", label: "Toggle context inspector" },
  { command: "toggleBottomPanel", label: "Toggle bottom panel" },
  { command: "toggleCommandPalette", label: "Open command palette" },
];

interface ShortcutSettingsDialogProps {
  bindings: ShortcutBindings;
  error: string | null;
  loading: boolean;
  onClose: () => void;
  onSave: (bindings: ShortcutBindings) => Promise<boolean>;
}

export function ShortcutSettingsDialog({ bindings, error, loading, onClose, onSave }: ShortcutSettingsDialogProps) {
  const [draft, setDraft] = useState<Record<ShortcutCommand, string>>({ ...bindings });
  const [saving, setSaving] = useState(false);
  const dialog = useRef<HTMLElement>(null);
  const dirty = useRef(false);
  const validation = useMemo(() => {
    const normalized = { ...draft };
    const invalid = fields.some(({ command }) => {
      const value = normalizeShortcut(draft[command]);
      if (value) normalized[command] = value;
      return value === null;
    });
    const conflicts = invalid ? [] : validateShortcutBindings(normalized);
    return { conflicts, invalid, normalized };
  }, [draft]);

  useEffect(() => {
    if (!dirty.current) setDraft({ ...bindings });
  }, [bindings]);

  useEffect(() => {
    const returnFocusTo = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    dialog.current?.querySelector<HTMLInputElement>("input")?.focus();
    return () => {
      if (returnFocusTo?.isConnected) returnFocusTo.focus();
    };
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !saving) onClose();
      if (event.key !== "Tab") return;
      const focusable = Array.from(dialog.current?.querySelectorAll<HTMLElement>(
        "button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex='-1'])",
      ) ?? []);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable.at(-1);
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose, saving]);

  const save = async () => {
    if (validation.invalid || validation.conflicts.length > 0 || saving) return;
    setSaving(true);
    const saved = await onSave(validation.normalized);
    setSaving(false);
    if (saved) onClose();
  };

  return (
    <div className="dialog-backdrop" role="presentation">
      <section aria-label="Keyboard shortcuts" aria-modal="true" className="shortcut-settings" ref={dialog} role="dialog">
        <header><div><p className="eyebrow">Settings</p><h2>Keyboard shortcuts</h2></div><button aria-label="Close keyboard shortcuts" className="icon-button" disabled={saving} onClick={onClose} type="button">×</button></header>
        <p>Use <code>Mod</code> for Command on macOS and Control on Linux. Shortcuts never override terminal or editor input.</p>
        <div className="shortcut-settings__fields">
          {fields.map(({ command, label }) => (
            <label key={command}>{label}<input disabled={loading || saving} onChange={(event) => {
              dirty.current = true;
              setDraft((current) => ({ ...current, [command]: event.target.value }));
            }} value={draft[command]} /></label>
          ))}
        </div>
        {validation.invalid ? <p role="alert">Use Mod plus one letter or number, with optional Shift.</p> : null}
        {validation.conflicts.map((conflict) => <p key={conflict.shortcut} role="alert">{conflict.shortcut} is assigned more than once.</p>)}
        {error ? <p role="alert">{error}</p> : null}
        <footer>
          <button className="button" disabled={loading || saving} onClick={() => {
            dirty.current = true;
            setDraft({ ...defaultShortcutBindings() });
          }} type="button">Restore defaults</button>
          <button className="button" disabled={saving} onClick={onClose} type="button">Cancel</button>
          <button className="button button--primary" disabled={loading || saving || validation.invalid || validation.conflicts.length > 0} onClick={() => void save()} type="button">{saving ? "Saving…" : "Save shortcuts"}</button>
        </footer>
      </section>
    </div>
  );
}
