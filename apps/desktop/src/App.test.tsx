import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { ProjectWindow } from "./components/ProjectWindow";
import type { AppearanceState } from "./hooks/useAppearance";
import type { DesktopHostClient, ProjectSelection, RecentProject, SystemSnapshot } from "./lib/system";
import type {
  TerminalCommandClient,
  TerminalOpened,
  TerminalReadResult,
  TerminalStatus,
} from "./lib/terminalClient";
import type { WindowLayoutClient } from "./lib/windowLayoutClient";

const connectedSnapshot: SystemSnapshot = {
  appVersion: "0.1.0",
  platform: "macos",
  architecture: "aarch64",
  windowLabel: "main",
  daemon: {
    status: "connected",
    detail: "Connected",
    storageStatus: "ready",
    storageSchemaVersion: 1,
  },
};
const project: ProjectSelection = {
  id: "0f9dfde8-55db-4b4e-bef4-9845eed998ad",
  name: "checkout-service",
  roots: ["/tmp/checkout-service"],
};
const appearance: AppearanceState = {
  theme: "system",
  resolvedTheme: "light",
  scale: 100,
  setTheme: vi.fn(),
  setScale: vi.fn(),
};
const windowLayoutClient: WindowLayoutClient = {
  load: vi.fn().mockResolvedValue(null),
  save: vi.fn().mockResolvedValue(undefined),
};

function clientWith(snapshot: SystemSnapshot, selection: ProjectSelection | null = null): DesktopHostClient {
  return {
    listRecentProjects: vi.fn().mockResolvedValue([]),
    openProject: vi.fn().mockResolvedValue(selection),
    openRecentProject: vi.fn().mockResolvedValue(selection ?? project),
    read: vi.fn().mockResolvedValue(snapshot),
    setProjectFavorite: vi.fn((_projectId: string, favorite: boolean) => Promise.resolve(favorite)),
    unlockStorage: vi.fn().mockResolvedValue({
      ...snapshot,
      daemon: { ...snapshot.daemon, storageStatus: "ready", storageSchemaVersion: 1 },
    }),
  };
}

const terminalOpened: TerminalOpened = {
  canonicalCwd: "/projects/checkout-service",
  processId: 902,
  runId: "run-shell-1",
  state: "running",
  terminalId: "shell-1",
};

function terminalClient(open: TerminalCommandClient["open"] = vi.fn(() => Promise.resolve(terminalOpened))): TerminalCommandClient {
  return {
    attach: vi.fn().mockResolvedValue(terminalOpened),
    close: vi.fn<TerminalCommandClient["close"]>((terminalId) => Promise.resolve({ exit: null, state: "closed", terminalId })),
    list: vi.fn().mockResolvedValue([]),
    open,
    read: vi.fn(() => new Promise<TerminalReadResult>(() => undefined)),
    resize: vi.fn<TerminalCommandClient["resize"]>((terminalId) => Promise.resolve({ terminalId })),
    state: vi.fn<TerminalCommandClient["state"]>((terminalId): Promise<TerminalStatus> => Promise.resolve({ exit: null, state: "running", terminalId })),
    write: vi.fn<TerminalCommandClient["write"]>((terminalId) => Promise.resolve({ terminalId })),
  };
}

describe("Maestro desktop bootstrap", () => {
  it("opens an independent native window from the welcome surface", async () => {
    const openNewWindow = vi.fn().mockResolvedValue("project-window-id");
    const user = userEvent.setup();
    render(
      <App
        hostClient={clientWith(connectedSnapshot)}
        layoutClient={windowLayoutClient}
        windowClient={{ openNewWindow }}
      />,
    );

    await user.click(await screen.findByRole("button", { name: "New Window" }));
    expect(openNewWindow).toHaveBeenCalledOnce();
  });

  it("opens a validated native project selection from the no-project state", async () => {
    const hostClient = clientWith(connectedSnapshot, project);
    const user = userEvent.setup();
    render(<App hostClient={hostClient} layoutClient={windowLayoutClient} />);

    expect(screen.getByText("Connecting to Maestro service…")).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Welcome to Maestro" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open Project…" }));

    expect(hostClient.openProject).toHaveBeenCalledOnce();
    expect(await screen.findByRole("heading", { name: "checkout-service" })).toBeInTheDocument();
  });

  it("recovers after project registration fails and allows an explicit retry", async () => {
    const hostClient = clientWith(connectedSnapshot, project);
    vi.mocked(hostClient.openProject)
      .mockRejectedValueOnce(new Error("registration timed out"))
      .mockResolvedValueOnce(project);
    const user = userEvent.setup();
    render(<App hostClient={hostClient} layoutClient={windowLayoutClient} />);

    const open = await screen.findByRole("button", { name: "Open Project…" });
    await user.click(open);
    expect(await screen.findByText(/could not open the selected project folder/i)).toBeInTheDocument();
    expect(open).toBeEnabled();

    await user.click(open);
    expect(hostClient.openProject).toHaveBeenCalledTimes(2);
    expect(await screen.findByRole("heading", { name: "checkout-service" })).toBeInTheDocument();
  });

  it("surfaces and retries project-open failures from an existing project window", async () => {
    const hostClient = clientWith(connectedSnapshot, project);
    vi.mocked(hostClient.openProject)
      .mockRejectedValueOnce(new Error("registration timed out"))
      .mockResolvedValueOnce(project);
    const user = userEvent.setup();
    render(<App hostClient={hostClient} initialProjectName="current-project" layoutClient={windowLayoutClient} />);

    await user.click(await screen.findByRole("button", { name: /Switch project, current project current-project/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/could not open the selected project folder/i);

    await user.click(screen.getByRole("button", { name: "Try another folder" }));
    expect(hostClient.openProject).toHaveBeenCalledTimes(2);
    expect(await screen.findByRole("heading", { name: "checkout-service" })).toBeInTheDocument();
  });

  it("reaches Welcome and Project while honestly showing that the daemon is offline", async () => {
    const snapshot: SystemSnapshot = {
      ...connectedSnapshot,
      daemon: {
        status: "notConnected",
        detail: "Service has not started.",
        storageStatus: "unavailable",
        storageSchemaVersion: null,
      },
    };
    const user = userEvent.setup();
    render(<App hostClient={clientWith(snapshot, project)} layoutClient={windowLayoutClient} />);

    expect(await screen.findByRole("heading", { name: "Welcome to Maestro" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Agent service offline");
    await user.click(screen.getByRole("button", { name: "Open Project…" }));
    expect(await screen.findByText("Daemon offline")).toBeInTheDocument();
  });

  it("uses a blocking recovery surface only when the desktop host command fails", async () => {
    const hostClient: DesktopHostClient = {
      listRecentProjects: vi.fn().mockResolvedValue([]),
      openProject: vi.fn().mockResolvedValue(null),
      openRecentProject: vi.fn().mockRejectedValue(new Error("Host unavailable")),
      read: vi.fn().mockRejectedValue(new Error("Host unavailable")),
      setProjectFavorite: vi.fn().mockRejectedValue(new Error("Host unavailable")),
      unlockStorage: vi.fn().mockRejectedValue(new Error("Host unavailable")),
    };
    render(<App hostClient={hostClient} layoutClient={windowLayoutClient} />);

    expect(await screen.findByRole("heading", { name: "Maestro could not connect to its service." })).toBeInTheDocument();
  });

  it("requires encrypted storage creation before exposing project controls", async () => {
    const locked: SystemSnapshot = {
      ...connectedSnapshot,
      daemon: {
        ...connectedSnapshot.daemon,
        storageStatus: "passphraseCreateRequired",
        storageSchemaVersion: null,
      },
    };
    const hostClient = clientWith(locked);
    const user = userEvent.setup();
    render(<App hostClient={hostClient} layoutClient={windowLayoutClient} />);

    expect(await screen.findByRole("heading", { name: "Protect Maestro with a passphrase" })).toBeInTheDocument();
    await user.type(screen.getByLabelText("Passphrase"), "foundation-passphrase");
    await user.type(screen.getByLabelText("Confirm passphrase"), "foundation-passphrase");
    await user.click(screen.getByRole("button", { name: "Create encrypted storage" }));

    expect(hostClient.unlockStorage).toHaveBeenCalledWith("foundation-passphrase");
    expect(await screen.findByRole("heading", { name: "Welcome to Maestro" })).toBeInTheDocument();
  });

  it("lists and safely reopens a persisted recent project through the desktop host", async () => {
    const recentProject: RecentProject = {
      projectId: project.id,
      displayName: project.name,
      canonicalRoots: project.roots,
      favorite: false,
      lastOpenedAt: "2026-08-05T08:00:00Z",
    };
    const hostClient = clientWith(connectedSnapshot);
    vi.mocked(hostClient.listRecentProjects).mockResolvedValue([recentProject]);
    const user = userEvent.setup();
    render(<App hostClient={hostClient} layoutClient={windowLayoutClient} />);

    await user.click(await screen.findByRole("button", { name: /checkout-service.*\/tmp\/checkout-service/ }));

    expect(hostClient.listRecentProjects).toHaveBeenCalledWith(20);
    expect(hostClient.openRecentProject).toHaveBeenCalledWith(project.id);
    expect(await screen.findByRole("heading", { name: "checkout-service" })).toBeInTheDocument();
  });

  it("changes a recent project's favorite only after an explicit action", async () => {
    const recentProject: RecentProject = {
      projectId: project.id,
      displayName: project.name,
      canonicalRoots: project.roots,
      favorite: false,
      lastOpenedAt: "2026-08-05T08:00:00Z",
    };
    const hostClient = clientWith(connectedSnapshot);
    vi.mocked(hostClient.listRecentProjects).mockResolvedValue([recentProject]);
    const user = userEvent.setup();
    render(<App hostClient={hostClient} layoutClient={windowLayoutClient} />);

    const favorite = await screen.findByRole("button", { name: "Add checkout-service to favorites" });
    expect(hostClient.setProjectFavorite).not.toHaveBeenCalled();
    await user.click(favorite);

    expect(hostClient.setProjectFavorite).toHaveBeenCalledWith(project.id, true);
    expect(await screen.findByRole("button", { name: "Remove checkout-service from favorites" })).toHaveAttribute("aria-pressed", "true");
  });
});

describe("desktop shell interactions", () => {
  it("filters and dispatches palette actions while trapping focus and suppressing global shortcuts", async () => {
    const user = userEvent.setup();
    render(<App hostClient={clientWith(connectedSnapshot)} initialProjectName="maestro-app" layoutClient={windowLayoutClient} />);
    await screen.findByRole("heading", { name: "maestro-app" });
    const invoker = screen.getByRole("button", { name: /Commands/ });
    invoker.focus();
    await user.click(invoker);

    const dialog = screen.getByRole("dialog", { name: "Command palette" });
    expect(document.querySelector(".project-window__content")).toHaveAttribute("inert");
    const search = within(dialog).getByRole("textbox", { name: "Search commands" });
    expect(search).toHaveFocus();
    await user.type(search, "context");
    const filteredAction = within(dialog).getByRole("button", { name: /Toggle Context Inspector/ });
    expect(filteredAction).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /Open Project/ })).not.toBeInTheDocument();
    filteredAction.focus();
    await user.tab();
    expect(search).toHaveFocus();
    await user.tab({ shift: true });
    expect(filteredAction).toHaveFocus();

    fireEvent.keyDown(window, { key: "j", metaKey: true });
    expect(document.querySelector(".bottom-panel")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(invoker).toHaveFocus();

    await user.click(invoker);
    await user.type(screen.getByRole("textbox", { name: "Search commands" }), "bottom");
    await user.click(screen.getByRole("button", { name: /Toggle Bottom Panel/ }));
    expect(screen.queryByRole("region", { name: "Console panel" })).not.toBeInTheDocument();
  });

  it("uses drawer state for compact layouts and F6 skips hidden zones", async () => {
    render(
      <ProjectWindow
        appearance={appearance}
        daemon={connectedSnapshot.daemon}
        layoutClient={windowLayoutClient}
        onOpenProject={vi.fn()}
        platform="macos"
        project={project}
        responsiveOverride={{ inspectorDrawer: true, sidebarDrawer: true }}
        windowId="responsive-test"
      />,
    );
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "Context inspector" })).not.toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Toggle context inspector" }));
    expect(screen.getByRole("complementary", { name: "Context inspector" })).toHaveClass("panel-drawer");
    expect(screen.queryByRole("complementary", { name: "Sessions sidebar" })).not.toBeInTheDocument();

    screen.getByRole("button", { name: /Commands/ }).focus();
    fireEvent.keyDown(window, { key: "F6" });
    expect(screen.getByRole("button", { name: "Sessions" })).toHaveFocus();
    fireEvent.keyDown(window, { key: "F6" });
    expect(screen.getByRole("tab", { name: /Foundation/ })).toHaveFocus();
  });

  it("implements roving keyboard behavior for workspace and console tabs", async () => {
    render(<App hostClient={clientWith(connectedSnapshot)} initialProjectName="maestro-app" layoutClient={windowLayoutClient} />);
    await screen.findByRole("heading", { name: "maestro-app" });

    const foundation = screen.getByRole("tab", { name: /Foundation/ });
    foundation.focus();
    fireEvent.keyDown(foundation, { key: "ArrowRight" });
    expect(screen.getByRole("tab", { name: "Plan" })).toHaveFocus();
    expect(screen.getByRole("tab", { name: "Plan" })).toHaveAttribute("aria-selected", "true");

    const events = screen.getByRole("tab", { name: /Events/ });
    events.focus();
    fireEvent.keyDown(events, { key: "End" });
    expect(screen.getByRole("tab", { name: "Shell" })).toHaveFocus();
    expect(screen.getByRole("tab", { name: "Shell" })).toHaveAttribute("aria-selected", "true");
  });

  it("opens a shell through its opaque project grant and never renders xterm before open succeeds", async () => {
    let resolveOpen: ((opened: TerminalOpened) => void) | undefined;
    const open = vi.fn(() => new Promise<TerminalOpened>((resolve) => { resolveOpen = resolve; }));
    const client = terminalClient(open);
    const view = render(
      <ProjectWindow
        appearance={appearance}
        daemon={connectedSnapshot.daemon}
        onOpenProject={vi.fn()}
        platform="macos"
        project={{ ...project, name: "Friendly project label" }}
        layoutClient={windowLayoutClient}
        terminalClient={client}
        windowId="terminal-open-test"
      />,
    );

    await userEvent.click(screen.getByRole("tab", { name: "Shell" }));
    await userEvent.click(screen.getByRole("button", { name: "Start Shell Terminal" }));
    expect(open).toHaveBeenCalledWith(project.id, 100, 30);
    expect(screen.getByText("Starting shell terminal…")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Shell terminal" })).not.toBeInTheDocument();

    act(() => resolveOpen?.(terminalOpened));
    expect(await screen.findByRole("region", { name: "Shell terminal" })).toBeInTheDocument();
    view.unmount();
    expect(client.close).not.toHaveBeenCalled();
  });

  it("leaves a late-opening daemon terminal running and discoverable after its project view is gone", async () => {
    let resolveOpen: ((opened: TerminalOpened) => void) | undefined;
    const client = terminalClient(vi.fn(() => new Promise<TerminalOpened>((resolve) => { resolveOpen = resolve; })));
    const view = render(
      <ProjectWindow
        appearance={appearance}
        daemon={connectedSnapshot.daemon}
        onOpenProject={vi.fn()}
        platform="macos"
        project={project}
        layoutClient={windowLayoutClient}
        terminalClient={client}
        windowId="stale-terminal-open-test"
      />,
    );

    await userEvent.click(screen.getByRole("tab", { name: "Shell" }));
    await userEvent.click(screen.getByRole("button", { name: "Start Shell Terminal" }));
    view.unmount();
    act(() => resolveOpen?.(terminalOpened));

    await act(async () => Promise.resolve());
    expect(client.close).not.toHaveBeenCalled();
    vi.mocked(client.list).mockResolvedValue([{
      ...terminalOpened,
      exit: null,
      kind: "shell",
      title: "Shell terminal",
    }]);
    expect(await client.list(project.id, 32)).toEqual([
      expect.objectContaining({ runId: "run-shell-1", state: "running" }),
    ]);
  });

  it("lists and reattaches an existing project shell without opening another process", async () => {
    const client = terminalClient();
    vi.mocked(client.list).mockResolvedValue([{
      ...terminalOpened,
      exit: null,
      kind: "shell",
      terminalId: "shell-reference-1",
      title: "Shell terminal",
    }]);
    vi.mocked(client.attach).mockResolvedValue({
      ...terminalOpened,
      terminalId: "shell-attached-grant-1",
    });
    render(
      <ProjectWindow
        appearance={appearance}
        daemon={connectedSnapshot.daemon}
        onOpenProject={vi.fn()}
        platform="macos"
        project={project}
        layoutClient={windowLayoutClient}
        terminalClient={client}
        windowId="terminal-attach-test"
      />,
    );

    await userEvent.click(screen.getByRole("tab", { name: "Shell" }));
    await userEvent.click(await screen.findByRole("button", { name: "Attach shell 902" }));

    expect(client.attach).toHaveBeenCalledWith(project.id, "shell-reference-1");
    expect(client.open).not.toHaveBeenCalled();
    expect(await screen.findByRole("region", { name: "Shell terminal" })).toBeInTheDocument();
  });

  it("closes a daemon terminal only from the explicit stop action", async () => {
    const client = terminalClient();
    render(
      <ProjectWindow
        appearance={appearance}
        daemon={connectedSnapshot.daemon}
        onOpenProject={vi.fn()}
        platform="macos"
        project={project}
        layoutClient={windowLayoutClient}
        terminalClient={client}
        windowId="terminal-stop-test"
      />,
    );

    await userEvent.click(screen.getByRole("tab", { name: "Shell" }));
    await userEvent.click(screen.getByRole("button", { name: "Start Shell Terminal" }));
    await screen.findByRole("region", { name: "Shell terminal" });
    await userEvent.click(screen.getByRole("button", { name: "Stop Terminal" }));

    await waitFor(() => expect(client.close).toHaveBeenCalledWith("shell-1"));
    expect(await screen.findByText("The shell terminal was stopped.")).toBeInTheDocument();
  });

  it("shows offline and open-error states instead of claiming the terminal is live", async () => {
    const offline = render(
      <ProjectWindow
        appearance={appearance}
        daemon={{
          detail: "Unavailable",
          status: "notConnected",
          storageStatus: "unavailable",
          storageSchemaVersion: null,
        }}
        onOpenProject={vi.fn()}
        platform="macos"
        project={project}
        layoutClient={windowLayoutClient}
        terminalClient={terminalClient()}
        windowId="terminal-offline-test"
      />,
    );
    await userEvent.click(screen.getByRole("tab", { name: "Shell" }));
    expect(screen.getByRole("button", { name: "Start Shell Terminal" })).toBeDisabled();
    offline.unmount();

    const failedClient = terminalClient(vi.fn().mockRejectedValue(new Error("PTY launch denied")));
    render(
      <ProjectWindow
        appearance={appearance}
        daemon={connectedSnapshot.daemon}
        onOpenProject={vi.fn()}
        platform="macos"
        project={project}
        layoutClient={windowLayoutClient}
        terminalClient={failedClient}
        windowId="terminal-failure-test"
      />,
    );
    await userEvent.click(screen.getByRole("tab", { name: "Shell" }));
    await userEvent.click(screen.getByRole("button", { name: "Start Shell Terminal" }));
    expect(await screen.findByText("PTY launch denied")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Shell terminal" })).not.toBeInTheDocument();
  });
});
