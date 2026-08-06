import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ProjectClient } from "../lib/project";
import type { ProjectSelection } from "../lib/system";
import { PrimarySidebar } from "./PrimarySidebar";

const firstRoot = "/workspaces/maestro-app";
const secondRoot = "/workspaces/maestro-docs";
const project: ProjectSelection = {
  id: "project-grant",
  name: "Maestro workspace",
  roots: [firstRoot, secondRoot],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, reject, resolve };
}

function directoryPage(directory: string, displayName: string) {
  return {
    directory,
    entries: [{
      bytes: 12,
      displayName,
      kind: "file" as const,
      path: `${directory}/${displayName}`,
    }],
    nextCursor: null,
  };
}

function statusEntry(path: string) {
  return {
    indexStatus: ".",
    kind: "ordinary" as const,
    originalPath: null,
    path: { bytes: [], display: path },
    worktreeStatus: "M",
  };
}

describe("PrimarySidebar multi-root project resources", () => {
  it("windows large file folders and pins the focused file while scrolling", async () => {
    const entries = Array.from({ length: 1_000 }, (_, index) => ({
      bytes: index,
      displayName: `file-${String(index).padStart(4, "0")}.ts`,
      kind: "file" as const,
      path: `${firstRoot}/file-${String(index).padStart(4, "0")}.ts`,
    }));
    const listDirectory = vi.fn<ProjectClient["listDirectory"]>(() => Promise.resolve({
      directory: firstRoot,
      entries,
      nextCursor: null,
    }));
    render(
      <PrimarySidebar
        activity="files"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={{ ...project, roots: [firstRoot] }}
        projectClient={{ listDirectory } as unknown as ProjectClient}
      />,
    );

    const list = await screen.findByRole("list", { name: "Project files" });
    await waitFor(() => expect(within(list).getAllByRole("listitem").length).toBeGreaterThan(0));
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(50);
    const focusedFile = within(list).getByRole("button", { name: /file-0000\.ts/ });
    focusedFile.focus();
    expect(focusedFile).toHaveFocus();

    list.scrollTop = 16_000;
    fireEvent.scroll(list);

    await waitFor(() => expect(within(list).getByRole("button", { name: /file-0000\.ts/ })).toBe(focusedFile));
    expect(focusedFile).toHaveFocus();
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(50);
  });

  it("reloads Files from the selected workspace root", async () => {
    const listDirectory = vi.fn<ProjectClient["listDirectory"]>((_projectGrant, directory) => Promise.resolve(
      directoryPage(directory, directory === firstRoot ? "main.ts" : "guide.md"),
    ));
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="files"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ listDirectory } as unknown as ProjectClient}
      />,
    );

    expect(await screen.findByRole("button", { name: /main\.ts/ })).toBeInTheDocument();
    await user.selectOptions(screen.getByRole("combobox", { name: "Workspace folder" }), secondRoot);

    expect(await screen.findByRole("button", { name: /guide\.md/ })).toBeInTheDocument();
    expect(listDirectory).toHaveBeenLastCalledWith(project.id, secondRoot);
    expect(screen.getByTitle(secondRoot)).toHaveTextContent(secondRoot);
  });

  it("refreshes Git from the selected repository root", async () => {
    const gitStatus = vi.fn<ProjectClient["gitStatus"]>((_projectGrant, repository) => Promise.resolve([
      statusEntry(repository === firstRoot ? "src/main.ts" : "docs/guide.md"),
    ]));
    const gitBranch = vi.fn<ProjectClient["gitBranch"]>((_projectGrant, repository) => Promise.resolve({
      data: repository === firstRoot ? "main" : "documentation",
      state: "branch",
    }));
    const gitWorktrees = vi.fn<ProjectClient["gitWorktrees"]>(() => Promise.resolve([]));
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="git"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ gitBranch, gitStatus, gitWorktrees } as unknown as ProjectClient}
      />,
    );

    expect(await screen.findByText("Branch: main")).toBeInTheDocument();
    gitBranch.mockClear();
    gitStatus.mockClear();
    gitWorktrees.mockClear();

    await user.selectOptions(screen.getByRole("combobox", { name: "Repository folder" }), secondRoot);

    expect(await screen.findByText("Branch: documentation")).toBeInTheDocument();
    await waitFor(() => {
      expect(gitStatus).toHaveBeenCalledWith(project.id, secondRoot);
      expect(gitBranch).toHaveBeenCalledWith(project.id, secondRoot);
      expect(gitWorktrees).toHaveBeenCalledWith(project.id, secondRoot);
    });
    expect(screen.getByRole("button", { name: /docs\/guide\.md/ })).toBeInTheDocument();
  });

  it.each([
    ["Literal text", "literal"],
    ["Regular expression", "regex"],
  ] as const)("dispatches %s repository searches through the existing contract", async (syntax, mode) => {
    const search = vi.fn<ProjectClient["search"]>(() => Promise.resolve({
      matches: [],
      summary: {
        cancelled: false,
        consumerStopped: false,
        limitReached: false,
        matches: 0,
        scannedFiles: 3,
        skippedFiles: 0,
      },
    }));
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="search"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ search } as unknown as ProjectClient}
      />,
    );

    await user.type(screen.getByRole("textbox", { name: "Search project" }), "agent.*session");
    await user.selectOptions(screen.getByRole("combobox", { name: "Search syntax" }), mode);
    await user.click(screen.getByRole("button", { name: "Search" }));

    await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
    expect(search).toHaveBeenCalledWith(project.id, expect.any(String), {
      caseSensitive: false,
      includeHidden: false,
      maximumFileBytes: 4 * 1024 * 1024,
      maximumResults: 500,
      mode,
      pattern: "agent.*session",
    });
    expect(screen.getByRole("option", { name: syntax })).toBeInTheDocument();
  });

  it("ignores Files results that finish after a newly selected root", async () => {
    const firstRequest = deferred<ReturnType<typeof directoryPage>>();
    const secondRequest = deferred<ReturnType<typeof directoryPage>>();
    const listDirectory = vi.fn<ProjectClient["listDirectory"]>((_projectGrant, directory) => (
      directory === firstRoot ? firstRequest.promise : secondRequest.promise
    ));
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="files"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ listDirectory } as unknown as ProjectClient}
      />,
    );

    await waitFor(() => expect(listDirectory).toHaveBeenCalledWith(project.id, firstRoot));
    await user.selectOptions(screen.getByRole("combobox", { name: "Workspace folder" }), secondRoot);
    await waitFor(() => expect(listDirectory).toHaveBeenCalledWith(project.id, secondRoot));
    await act(async () => {
      secondRequest.resolve(directoryPage(secondRoot, "guide.md"));
      await secondRequest.promise;
    });
    expect(await screen.findByRole("button", { name: /guide\.md/ })).toBeInTheDocument();

    await act(async () => {
      firstRequest.resolve(directoryPage(firstRoot, "stale.ts"));
      await firstRequest.promise;
    });
    expect(screen.queryByRole("button", { name: /stale\.ts/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /guide\.md/ })).toBeInTheDocument();
    expect(screen.queryByText("Loading…")).not.toBeInTheDocument();
  });

  it("ignores Files errors that arrive from a previous root", async () => {
    const firstRequest = deferred<ReturnType<typeof directoryPage>>();
    const listDirectory = vi.fn<ProjectClient["listDirectory"]>((_projectGrant, directory) => (
      directory === firstRoot
        ? firstRequest.promise
        : Promise.resolve(directoryPage(secondRoot, "guide.md"))
    ));
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="files"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ listDirectory } as unknown as ProjectClient}
      />,
    );

    await waitFor(() => expect(listDirectory).toHaveBeenCalledWith(project.id, firstRoot));
    await user.selectOptions(screen.getByRole("combobox", { name: "Workspace folder" }), secondRoot);
    expect(await screen.findByRole("button", { name: /guide\.md/ })).toBeInTheDocument();
    await act(async () => {
      firstRequest.reject(new Error("old root disappeared"));
      await firstRequest.promise.catch(() => undefined);
    });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("keeps newer Git results when the previous root finishes last", async () => {
    const firstStatus = deferred<ReturnType<typeof statusEntry>[]>();
    const firstBranch = deferred<{ data: string; state: "branch" }>();
    const firstWorktrees = deferred<[]>();
    const secondStatus = deferred<ReturnType<typeof statusEntry>[]>();
    const secondBranch = deferred<{ data: string; state: "branch" }>();
    const secondWorktrees = deferred<[]>();
    const gitStatus = vi.fn<ProjectClient["gitStatus"]>((_projectGrant, repository) => (
      repository === firstRoot ? firstStatus.promise : secondStatus.promise
    ));
    const gitBranch = vi.fn<ProjectClient["gitBranch"]>((_projectGrant, repository) => (
      repository === firstRoot ? firstBranch.promise : secondBranch.promise
    ));
    const gitWorktrees = vi.fn<ProjectClient["gitWorktrees"]>((_projectGrant, repository) => (
      repository === firstRoot ? firstWorktrees.promise : secondWorktrees.promise
    ));
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="git"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ gitBranch, gitStatus, gitWorktrees } as unknown as ProjectClient}
      />,
    );

    await waitFor(() => expect(gitStatus).toHaveBeenCalledWith(project.id, firstRoot));
    await user.selectOptions(screen.getByRole("combobox", { name: "Repository folder" }), secondRoot);
    await waitFor(() => expect(gitStatus).toHaveBeenCalledWith(project.id, secondRoot));
    await act(async () => {
      secondStatus.resolve([statusEntry("docs/guide.md")]);
      secondBranch.resolve({ data: "documentation", state: "branch" });
      secondWorktrees.resolve([]);
      await Promise.all([secondStatus.promise, secondBranch.promise, secondWorktrees.promise]);
    });
    expect(await screen.findByText("Branch: documentation")).toBeInTheDocument();

    await act(async () => {
      firstStatus.resolve([statusEntry("src/stale.ts")]);
      firstBranch.resolve({ data: "stale-main", state: "branch" });
      firstWorktrees.resolve([]);
      await Promise.all([firstStatus.promise, firstBranch.promise, firstWorktrees.promise]);
    });
    expect(screen.queryByText("Branch: stale-main")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /src\/stale\.ts/ })).not.toBeInTheDocument();
    expect(screen.getByText("Branch: documentation")).toBeInTheDocument();
    expect(screen.queryByText("Loading Git information…")).not.toBeInTheDocument();
  });

  it("ignores Git errors that arrive from a previous repository root", async () => {
    const firstStatus = deferred<ReturnType<typeof statusEntry>[]>();
    const gitStatus = vi.fn<ProjectClient["gitStatus"]>((_projectGrant, repository) => (
      repository === firstRoot ? firstStatus.promise : Promise.resolve([statusEntry("docs/guide.md")])
    ));
    const gitBranch = vi.fn<ProjectClient["gitBranch"]>((_projectGrant, repository) => Promise.resolve({
      data: repository === firstRoot ? "main" : "documentation",
      state: "branch",
    }));
    const gitWorktrees = vi.fn<ProjectClient["gitWorktrees"]>(() => Promise.resolve([]));
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="git"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ gitBranch, gitStatus, gitWorktrees } as unknown as ProjectClient}
      />,
    );

    await waitFor(() => expect(gitStatus).toHaveBeenCalledWith(project.id, firstRoot));
    await user.selectOptions(screen.getByRole("combobox", { name: "Repository folder" }), secondRoot);
    expect(await screen.findByText("Branch: documentation")).toBeInTheDocument();
    await act(async () => {
      firstStatus.reject(new Error("old repository unavailable"));
      await firstStatus.promise.catch(() => undefined);
    });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /docs\/guide\.md/ })).toBeInTheDocument();
  });

  it("shows worktree paths, states, and maintenance reasons", async () => {
    const gitStatus = vi.fn<ProjectClient["gitStatus"]>(() => Promise.resolve([]));
    const gitBranch = vi.fn<ProjectClient["gitBranch"]>(() => Promise.resolve({ data: "main", state: "branch" }));
    const gitWorktrees = vi.fn<ProjectClient["gitWorktrees"]>(() => Promise.resolve([
      {
        bare: false,
        branch: "refs/heads/main",
        detached: false,
        head: "abc123",
        lockedReason: "in use by an agent",
        path: "/worktrees/maestro-main",
        prunableReason: null,
      },
      {
        bare: false,
        branch: null,
        detached: true,
        head: "def456",
        lockedReason: null,
        path: "/worktrees/maestro-review",
        prunableReason: "gitdir file points to a missing location",
      },
      {
        bare: true,
        branch: null,
        detached: false,
        head: null,
        lockedReason: null,
        path: "/repositories/maestro.git",
        prunableReason: null,
      },
    ]));
    render(
      <PrimarySidebar
        activity="git"
        onClose={vi.fn()}
        onOpenDiff={vi.fn()}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ gitBranch, gitStatus, gitWorktrees } as unknown as ProjectClient}
      />,
    );

    expect(await screen.findByRole("heading", { name: /Worktrees 3/ })).toBeInTheDocument();
    expect(screen.getByText("/worktrees/maestro-main")).toBeInTheDocument();
    expect(screen.getByText("Branch refs/heads/main")).toBeInTheDocument();
    expect(screen.getByText("Detached HEAD")).toBeInTheDocument();
    expect(screen.getByText("Bare repository")).toBeInTheDocument();
    expect(screen.getByText("in use by an agent")).toBeInTheDocument();
    expect(screen.getByText("gitdir file points to a missing location")).toBeInTheDocument();
  });

  it("surfaces Git diff failures without dispatching a broken diff", async () => {
    const gitStatus = vi.fn<ProjectClient["gitStatus"]>(() => Promise.resolve([]));
    const gitBranch = vi.fn<ProjectClient["gitBranch"]>(() => Promise.resolve({ data: "main", state: "branch" }));
    const gitWorktrees = vi.fn<ProjectClient["gitWorktrees"]>(() => Promise.resolve([]));
    const gitDiff = vi.fn<ProjectClient["gitDiff"]>(() => Promise.reject(new Error("git diff failed")));
    const onOpenDiff = vi.fn();
    const user = userEvent.setup();
    render(
      <PrimarySidebar
        activity="git"
        onClose={vi.fn()}
        onOpenDiff={onOpenDiff}
        onOpenFile={vi.fn()}
        open
        project={project}
        projectClient={{ gitBranch, gitDiff, gitStatus, gitWorktrees } as unknown as ProjectClient}
      />,
    );

    await screen.findByText("Branch: main");
    await user.click(screen.getByRole("button", { name: "View diff" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The working tree diff could not be loaded.");
    expect(onOpenDiff).not.toHaveBeenCalled();
  });
});
