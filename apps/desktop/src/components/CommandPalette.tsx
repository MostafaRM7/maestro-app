import { useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "./Icon";

export interface CommandPaletteAction {
  id: string;
  label: string;
  run: () => void | Promise<void>;
  shortcut?: string;
}

interface CommandPaletteProps {
  actions: readonly CommandPaletteAction[];
  onClose: () => void;
}

const FOCUSABLE = "input:not([disabled]), button:not([disabled]), [href], [tabindex]:not([tabindex='-1'])";

export function CommandPalette({ actions, onClose }: CommandPaletteProps) {
  const dialogRef = useRef<HTMLElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const invokerRef = useRef<HTMLElement | null>(document.activeElement as HTMLElement | null);
  const [query, setQuery] = useState("");
  const filteredActions = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return normalized
      ? actions.filter((action) => action.label.toLocaleLowerCase().includes(normalized))
      : actions;
  }, [actions, query]);

  useEffect(() => {
    const invoker = invokerRef.current;
    searchRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;

      const focusable = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? []);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      invoker?.focus();
    };
  }, [onClose]);

  function execute(action: CommandPaletteAction) {
    onClose();
    queueMicrotask(() => void action.run());
  }

  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={onClose}>
      <section
        ref={dialogRef}
        aria-label="Command palette"
        aria-modal="true"
        className="command-palette"
        onMouseDown={(event) => event.stopPropagation()}
        role="dialog"
      >
        <div className="command-palette__search">
          <Icon name="search" />
          <input
            ref={searchRef}
            aria-label="Search commands"
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Type a command…"
            value={query}
          />
          <kbd>Esc</kbd>
        </div>
        <p className="command-group-label">Available commands</p>
        <p className="visually-hidden" aria-live="polite">{filteredActions.length} commands available</p>
        {filteredActions.length > 0 ? (
          <ul>
            {filteredActions.map((action) => (
              <li key={action.id}>
                <button type="button" onClick={() => execute(action)}>
                  <span>{action.label}</span>{action.shortcut ? <kbd>{action.shortcut}</kbd> : null}
                </button>
              </li>
            ))}
          </ul>
        ) : <p className="command-palette__empty">No commands match “{query}”.</p>}
      </section>
    </div>
  );
}
