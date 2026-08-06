import { describe, expect, it } from "vitest";
import { createTauriDesktopHostClient } from "./system";

describe("Tauri desktop host client", () => {
  it("uses narrow recent-project and favorite command contracts", async () => {
    const calls: unknown[][] = [];
    const client = createTauriDesktopHostClient(<T>(command: string, args?: Record<string, unknown>) => {
      calls.push([command, args]);
      return Promise.resolve(undefined as T);
    });

    await client.listRecentProjects(20);
    await client.openRecentProject("project-1");
    await client.setProjectFavorite("project-1", true);

    expect(calls).toEqual([
      ["project_recent_list", { maximumProjects: 20 }],
      ["open_recent_project", { projectId: "project-1" }],
      ["project_set_favorite", { favorite: true, projectId: "project-1" }],
    ]);
  });
});
