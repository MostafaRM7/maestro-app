import { describe, expect, it, vi } from "vitest";
import { createNativeWindowClient } from "./windowClient";

describe("native window client", () => {
  it("uses the narrow desktop command without supplying window authority", async () => {
    const invoke = vi.fn().mockResolvedValue("project-window-id");
    const client = createNativeWindowClient(invoke);

    await expect(client.openNewWindow()).resolves.toBe("project-window-id");
    expect(invoke).toHaveBeenCalledWith("open_new_window");
  });
});
