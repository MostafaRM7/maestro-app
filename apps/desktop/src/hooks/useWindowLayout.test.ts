import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WindowLayoutClient } from "../lib/windowLayoutClient";
import {
  DEFAULT_WINDOW_LAYOUT,
  parseWindowLayout,
  useWindowLayout,
  windowLayoutStorageKey,
} from "./useWindowLayout";

function layoutClient(stored: string | null = null): WindowLayoutClient {
  return {
    load: vi.fn().mockResolvedValue(stored),
    save: vi.fn().mockResolvedValue(undefined),
  };
}

describe("encrypted daemon-backed window layout state", () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
  afterEach(() => vi.useRealTimers());

  it("isolates layout identities by opaque project grant and native window identity", () => {
    expect(windowLayoutStorageKey("grant-alpha", "main"))
      .not.toBe(windowLayoutStorageKey("grant-beta", "main"));
    expect(windowLayoutStorageKey("grant-alpha", "main"))
      .not.toBe(windowLayoutStorageKey("grant-alpha", "detached"));
  });

  it("loads a stored layout and debounces subsequent saves", async () => {
    const client = layoutClient(JSON.stringify({ ...DEFAULT_WINDOW_LAYOUT, sidebarOpen: false }));
    const view = renderHook(() => useWindowLayout("project-1", "main", client));
    await act(() => Promise.resolve());

    expect(client.load).toHaveBeenCalledWith("project-1", "main");
    expect(view.result.current.hydrated).toBe(true);
    expect(view.result.current.layout.sidebarOpen).toBe(false);

    act(() => view.result.current.update({ sidebarWidth: 320 }));
    await vi.advanceTimersByTimeAsync(299);
    expect(client.save).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);

    expect(client.save).toHaveBeenCalledWith(
      "project-1",
      "main",
      JSON.stringify({ ...DEFAULT_WINDOW_LAYOUT, sidebarOpen: false, sidebarWidth: 320 }),
    );
  });

  it("does not overwrite encrypted state when loading fails", async () => {
    const client = layoutClient();
    vi.mocked(client.load).mockRejectedValue(new Error("daemon unavailable"));
    const view = renderHook(() => useWindowLayout("project-1", "main", client));
    await act(() => Promise.resolve());

    act(() => view.result.current.update({ sidebarOpen: false }));
    await vi.advanceTimersByTimeAsync(1_000);

    expect(view.result.current.hydrated).toBe(false);
    expect(client.save).not.toHaveBeenCalled();
  });

  it("flushes the latest debounced layout when its native window unmounts", async () => {
    const client = layoutClient();
    const view = renderHook(() => useWindowLayout("project-1", "main", client));
    await act(() => Promise.resolve());
    act(() => view.result.current.update({ inspectorOpen: false }));
    view.unmount();

    expect(client.save).toHaveBeenCalledWith(
      "project-1",
      "main",
      JSON.stringify({ ...DEFAULT_WINDOW_LAYOUT, inspectorOpen: false }),
    );
  });

  it("rejects invalid enums and clamps unsafe panel dimensions", () => {
    const layout = parseWindowLayout(JSON.stringify({
      activity: "invalid",
      bottomHeight: 9_999,
      inspectorWidth: 1,
      sidebarWidth: Number.NaN,
    }));

    expect(layout.activity).toBe(DEFAULT_WINDOW_LAYOUT.activity);
    expect(layout.bottomHeight).toBe(600);
    expect(layout.inspectorWidth).toBe(240);
    expect(layout.sidebarWidth).toBe(DEFAULT_WINDOW_LAYOUT.sidebarWidth);
  });
});
