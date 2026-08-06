import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useGlobalShortcuts, type ShortcutActions } from "./useGlobalShortcuts";

function Harness({ actions, platform = "linux" }: { actions: ShortcutActions; platform?: string }) {
  useGlobalShortcuts(actions, { enabled: true, platform });
  return (
    <div>
      <textarea aria-label="Editor" />
      <div data-terminal-input="true"><textarea aria-label="Terminal input" /></div>
      <button type="button">Neutral target</button>
    </div>
  );
}

function actions(): ShortcutActions {
  return {
    cycleFocus: vi.fn(),
    openNewWindow: vi.fn(),
    openProject: vi.fn(),
    toggleBottomPanel: vi.fn(),
    toggleCommandPalette: vi.fn(),
    toggleInspector: vi.fn(),
    toggleSidebar: vi.fn(),
  };
}

describe("global shortcuts", () => {
  it("never steals Linux control keys from terminal or editor-owned targets", () => {
    const callbacks = actions();
    const view = render(<Harness actions={callbacks} />);
    const editor = view.getByRole("textbox", { name: "Editor" });
    const terminal = view.getByRole("textbox", { name: "Terminal input" });

    fireEvent.keyDown(editor, { key: "b", ctrlKey: true });
    fireEvent.keyDown(editor, { key: "j", ctrlKey: true });
    fireEvent.keyDown(editor, { key: "F6" });
    fireEvent.keyDown(terminal, { key: "o", ctrlKey: true });
    fireEvent.keyDown(terminal, { key: "F6" });

    expect(callbacks.toggleSidebar).not.toHaveBeenCalled();
    expect(callbacks.toggleBottomPanel).not.toHaveBeenCalled();
    expect(callbacks.openProject).not.toHaveBeenCalled();
    expect(callbacks.cycleFocus).not.toHaveBeenCalled();
  });

  it("dispatches configured commands from neutral targets and preserves F6 navigation", () => {
    const callbacks = actions();
    const view = render(<Harness actions={callbacks} />);
    const neutral = view.getByRole("button", { name: "Neutral target" });

    fireEvent.keyDown(neutral, { key: "j", ctrlKey: true });
    fireEvent.keyDown(neutral, { key: "F6", shiftKey: true });

    expect(callbacks.toggleBottomPanel).toHaveBeenCalledOnce();
    expect(callbacks.cycleFocus).toHaveBeenCalledWith(true);
  });

  it("supports validated remapping without dispatching the old chord", () => {
    const callbacks = actions();
    const view = render(
      <HarnessWithBindings actions={callbacks} />,
    );
    const neutral = view.getByRole("button", { name: "Remap target" });

    fireEvent.keyDown(neutral, { key: "j", ctrlKey: true });
    fireEvent.keyDown(neutral, { key: "k", ctrlKey: true });

    expect(callbacks.toggleBottomPanel).toHaveBeenCalledOnce();
  });
});

function HarnessWithBindings({ actions: callbacks }: { actions: ShortcutActions }) {
  useGlobalShortcuts(callbacks, {
    bindings: {
      openNewWindow: "Mod+Shift+N",
      openProject: "Mod+O",
      toggleBottomPanel: "Mod+K",
      toggleCommandPalette: "Mod+Shift+P",
      toggleInspector: "Mod+Shift+B",
      toggleSidebar: "Mod+B",
    },
    enabled: true,
    platform: "linux",
  });
  return <button type="button">Remap target</button>;
}
