import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ShortcutSettingsClient } from "../lib/shortcutSettingsClient";
import { defaultShortcutBindings } from "../lib/shortcuts";
import { useShortcutSettings } from "./useShortcutSettings";

describe("useShortcutSettings", () => {
  it("loads validated encrypted settings and saves conflict-free remapping", async () => {
    const stored = { ...defaultShortcutBindings(), toggleBottomPanel: "Mod+K" };
    const client: ShortcutSettingsClient = {
      load: vi.fn().mockResolvedValue(stored),
      save: vi.fn().mockResolvedValue(undefined),
    };
    const { result } = renderHook(() => useShortcutSettings(client));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.bindings.toggleBottomPanel).toBe("Mod+K");
    const changed = { ...stored, openProject: "Mod+L" };
    await act(async () => expect(await result.current.save(changed)).toBe(true));
    expect(client.save).toHaveBeenCalledWith(changed);
    expect(result.current.bindings.openProject).toBe("Mod+L");
  });

  it("rejects invalid/conflicting stored and edited settings", async () => {
    const client: ShortcutSettingsClient = {
      load: vi.fn().mockResolvedValue({ ...defaultShortcutBindings(), openProject: "Ctrl+O" }),
      save: vi.fn().mockResolvedValue(undefined),
    };
    const { result } = renderHook(() => useShortcutSettings(client));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.bindings).toEqual(defaultShortcutBindings());
    expect(result.current.error).toMatch(/invalid/u);

    const conflicting = { ...defaultShortcutBindings(), openProject: "Mod+B" };
    await act(async () => expect(await result.current.save(conflicting)).toBe(false));
    expect(client.save).not.toHaveBeenCalled();
  });
});

