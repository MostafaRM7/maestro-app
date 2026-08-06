import { describe, expect, it } from "vitest";
import { createTauriWindowLayoutClient } from "./windowLayoutClient";

describe("Tauri window layout client", () => {
  it("uses the opaque project grant and exact native window key", async () => {
    const calls: unknown[][] = [];
    const client = createTauriWindowLayoutClient(<T>(command: string, args?: Record<string, unknown>) => {
      calls.push([command, args]);
      return Promise.resolve(undefined as T);
    });
    const layoutJson = JSON.stringify({ sidebarOpen: false });

    await client.load("project-grant-1", "main");
    await client.save("project-grant-1", "main", layoutJson);

    expect(calls).toEqual([
      ["project_window_layout_load", { projectGrant: "project-grant-1", windowKey: "main" }],
      ["project_window_layout_save", { layoutJson, projectGrant: "project-grant-1", windowKey: "main" }],
    ]);
  });
});
