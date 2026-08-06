import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { AppearanceState } from "../hooks/useAppearance";
import type { FakeSessionClient } from "../lib/fakeSession";
import type { ProjectClient } from "../lib/project";
import type { ProjectSelection } from "../lib/system";
import type { WindowLayoutClient } from "../lib/windowLayoutClient";
import { ProjectWindow } from "./ProjectWindow";

const project: ProjectSelection = {
  id: "project-grant",
  name: "maestro-app",
  roots: ["/workspaces/maestro-app"],
};
const appearance: AppearanceState = {
  resolvedTheme: "light",
  scale: 100,
  setScale: vi.fn(),
  setTheme: vi.fn(),
  theme: "system",
};
const layoutClient: WindowLayoutClient = {
  load: vi.fn().mockResolvedValue(null),
  save: vi.fn().mockResolvedValue(undefined),
};

describe("ProjectWindow lightweight editor", () => {
  it("dispatches Open Externally through the project-scoped client", async () => {
    const path = `${project.roots[0]}/README.md`;
    const listDirectory = vi.fn<ProjectClient["listDirectory"]>(() => Promise.resolve({
      directory: project.roots[0],
      entries: [{ bytes: 18, displayName: "README.md", kind: "file", path }],
      nextCursor: null,
    }));
    const readFile = vi.fn<ProjectClient["readFile"]>(() => Promise.resolve({
      bytes: 18,
      fingerprint: [1, 2, 3],
      path,
      text: "# Maestro\n",
    }));
    const openFileExternal = vi.fn<ProjectClient["openFileExternal"]>(() => Promise.resolve());
    const projectClient = { listDirectory, openFileExternal, readFile } as unknown as ProjectClient;
    const fakeSessionClient = {
      listSessions: vi.fn().mockResolvedValue([]),
    } as unknown as FakeSessionClient;
    const user = userEvent.setup();
    render(
      <ProjectWindow
        appearance={appearance}
        daemon={{
          detail: "Connected",
          status: "connected",
          storageSchemaVersion: 3,
          storageStatus: "ready",
        }}
        fakeSessionClient={fakeSessionClient}
        layoutClient={layoutClient}
        onOpenProject={vi.fn()}
        platform="macos"
        project={project}
        projectClient={projectClient}
        windowId="external-editor-test"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Files" }));
    await user.click(await screen.findByRole("button", { name: /README\.md/ }));
    expect(await screen.findByText("Lightweight editor")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Open Externally" }));

    await waitFor(() => expect(openFileExternal).toHaveBeenCalledWith(project.id, path));
  });
});
