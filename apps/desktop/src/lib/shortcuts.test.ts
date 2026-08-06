import { describe, expect, it } from "vitest";
import {
  defaultShortcutBindings,
  normalizeShortcut,
  shortcutLabel,
  shortcutMatches,
  validateShortcutBindings,
} from "./shortcuts";

describe("shortcut registry", () => {
  it("normalizes supported chords and rejects unsafe or modifier-free input", () => {
    expect(normalizeShortcut("mod + shift + n")).toBe("Mod+Shift+N");
    expect(normalizeShortcut("Mod+O")).toBe("Mod+O");
    expect(normalizeShortcut("Ctrl+O")).toBeNull();
    expect(normalizeShortcut("O")).toBeNull();
    expect(normalizeShortcut("Mod+Escape")).toBeNull();
  });

  it("detects conflicts and uses platform-specific labels", () => {
    const bindings = { ...defaultShortcutBindings(), toggleSidebar: "Mod+J" };
    expect(validateShortcutBindings(bindings)).toEqual([{
      commands: ["toggleBottomPanel", "toggleSidebar"],
      shortcut: "Mod+J",
    }]);
    expect(shortcutLabel("Mod+Shift+N", "macos")).toBe("⌘⇧N");
    expect(shortcutLabel("Mod+Shift+N", "linux")).toBe("Ctrl+Shift+N");
  });

  it("matches Mod to the active platform without accepting extra modifiers", () => {
    expect(shortcutMatches(new KeyboardEvent("keydown", { key: "o", metaKey: true }), "Mod+O", "macos")).toBe(true);
    expect(shortcutMatches(new KeyboardEvent("keydown", { key: "o", ctrlKey: true }), "Mod+O", "linux")).toBe(true);
    expect(shortcutMatches(new KeyboardEvent("keydown", { key: "o", ctrlKey: true, altKey: true }), "Mod+O", "linux")).toBe(false);
  });
});

