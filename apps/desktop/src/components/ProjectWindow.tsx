import { useCallback, useEffect, useMemo, useState, type CSSProperties } from "react";
import type { AppearanceState } from "../hooks/useAppearance";
import { useGlobalShortcuts } from "../hooks/useGlobalShortcuts";
import { useFakeSession } from "../hooks/useFakeSession";
import { useFakeTui } from "../hooks/useFakeTui";
import { useResponsivePanels, type ResponsivePanels } from "../hooks/useResponsivePanels";
import { useWindowLayout } from "../hooks/useWindowLayout";
import { useShellTerminal } from "../hooks/useShellTerminal";
import { useShortcutSettings } from "../hooks/useShortcutSettings";
import {
  tauriProjectClient,
  type GitDiff,
  type ProjectClient,
  type TextFile,
} from "../lib/project";
import type { DaemonSnapshot, ProjectSelection } from "../lib/system";
import { tauriFakeSessionClient, type FakeSessionClient } from "../lib/fakeSession";
import { tauriTerminalCommandClient, type TerminalCommandClient } from "../lib/terminalClient";
import type { WindowLayoutClient } from "../lib/windowLayoutClient";
import { shortcutLabel } from "../lib/shortcuts";
import type { ShortcutSettingsClient } from "../lib/shortcutSettingsClient";
import { ActivityRail } from "./ActivityRail";
import { BottomPanel } from "./BottomPanel";
import { CommandPalette, type CommandPaletteAction } from "./CommandPalette";
import { ContextInspector } from "./ContextInspector";
import { PanelResizeHandle } from "./PanelResizeHandle";
import { PrimarySidebar } from "./PrimarySidebar";
import { ProjectToolbar } from "./ProjectToolbar";
import { StatusBar } from "./StatusBar";
import { ShortcutSettingsDialog } from "./ShortcutSettingsDialog";
import { Workspace } from "./Workspace";

interface ProjectWindowProps {
  appearance: AppearanceState;
  daemon: DaemonSnapshot;
  fakeSessionClient?: FakeSessionClient;
  onOpenProject: () => void;
  onOpenNewWindow?: () => void;
  openingProject?: boolean;
  platform: string;
  project: ProjectSelection;
  projectError?: string | null;
  projectClient?: ProjectClient;
  layoutClient?: WindowLayoutClient;
  responsiveOverride?: ResponsivePanels;
  shortcutSettingsClient?: ShortcutSettingsClient;
  terminalClient?: TerminalCommandClient;
  windowId?: string;
}

const FOCUSABLE = "button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])";

export function ProjectWindow({
  appearance,
  daemon,
  fakeSessionClient = tauriFakeSessionClient,
  layoutClient,
  onOpenProject,
  onOpenNewWindow = () => {},
  openingProject = false,
  platform,
  project,
  projectError = null,
  projectClient = tauriProjectClient,
  responsiveOverride,
  shortcutSettingsClient,
  terminalClient = tauriTerminalCommandClient,
  windowId = "main",
}: ProjectWindowProps) {
  const { layout, update } = useWindowLayout(project.id, windowId, layoutClient);
  const responsive = useResponsivePanels(responsiveOverride);
  const [commandPaletteOpen, setCommandPaletteOpen] = useState(false);
  const [shortcutSettingsOpen, setShortcutSettingsOpen] = useState(false);
  const [file, setFile] = useState<TextFile | null>(null);
  const [draft, setDraft] = useState("");
  const [diff, setDiff] = useState<GitDiff | null>(null);
  const [resourceError, setResourceError] = useState<string | null>(null);
  const [resourceLoading, setResourceLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const shellTerminal = useShellTerminal(project.id, terminalClient);
  const fakeSession = useFakeSession(
    project.id,
    fakeSessionClient,
    layout.bottomPanelOpen && layout.bottomSurface === "raw",
  );
  const fakeTui = useFakeTui(project.id, fakeSessionClient, terminalClient);
  const shortcutSettings = useShortcutSettings(shortcutSettingsClient);

  const openFile = useCallback(async (path: string) => {
    setResourceLoading(true);
    setResourceError(null);
    try {
      const opened = await projectClient.readFile(project.id, path);
      setFile(opened);
      setDraft(opened.text);
      setDiff(null);
      update({ workspaceSurface: "conversation" });
    } catch {
      setResourceError("Maestro could not open this file as bounded UTF-8 text.");
    } finally {
      setResourceLoading(false);
    }
  }, [project.id, projectClient, update]);

  const openDiff = useCallback((opened: GitDiff) => {
    setDiff(opened);
    setFile(null);
    setResourceError(null);
    update({ workspaceSurface: "conversation" });
  }, [update]);

  const openCompatibilityTui = useCallback(() => {
    update({ bottomPanelOpen: true, bottomSurface: "agent" });
  }, [update]);

  const saveFile = useCallback(async () => {
    if (!file || saving) return;
    setSaving(true);
    setResourceError(null);
    try {
      const saved = await projectClient.saveFile(project.id, file.path, draft, file.fingerprint);
      setFile({ ...file, text: draft, fingerprint: saved.fingerprint, bytes: saved.bytes });
    } catch {
      setResourceError("The file changed on disk or could not be saved safely. Reload it before retrying.");
    } finally {
      setSaving(false);
    }
  }, [draft, file, project.id, projectClient, saving]);

  const openFileExternal = useCallback(async () => {
    if (!file) return;
    setResourceError(null);
    try {
      await projectClient.openFileExternal(project.id, file.path);
    } catch {
      setResourceError("Maestro could not open this file in the configured external application.");
    }
  }, [file, project.id, projectClient]);

  useEffect(() => {
    if (responsive.sidebarDrawer && responsive.inspectorDrawer && layout.sidebarOpen && layout.inspectorOpen) {
      update({ inspectorOpen: false });
    }
  }, [layout.inspectorOpen, layout.sidebarOpen, responsive.inspectorDrawer, responsive.sidebarDrawer, update]);

  const toggleSidebar = useCallback(() => {
    const sidebarOpen = !layout.sidebarOpen;
    update({
      sidebarOpen,
      ...(responsive.sidebarDrawer && sidebarOpen ? { inspectorOpen: false } : {}),
    });
  }, [layout.sidebarOpen, responsive.sidebarDrawer, update]);

  const toggleInspector = useCallback(() => {
    const inspectorOpen = !layout.inspectorOpen;
    update({
      inspectorOpen,
      ...(responsive.inspectorDrawer && inspectorOpen ? { sidebarOpen: false } : {}),
    });
  }, [layout.inspectorOpen, responsive.inspectorDrawer, update]);

  const cycleFocus = useCallback((backwards: boolean) => {
    const zones = Array.from(document.querySelectorAll<HTMLElement>("[data-focus-zone]"))
      .filter((zone) => {
        const style = getComputedStyle(zone);
        return !zone.hidden && zone.getAttribute("aria-hidden") !== "true"
          && style.display !== "none" && style.visibility !== "hidden";
      });
    if (zones.length === 0) return;

    const activeZone = document.activeElement?.closest<HTMLElement>("[data-focus-zone]");
    const activeIndex = activeZone ? zones.indexOf(activeZone) : -1;
    const offset = backwards ? -1 : 1;
    const nextIndex = activeIndex < 0
      ? (backwards ? zones.length - 1 : 0)
      : (activeIndex + offset + zones.length) % zones.length;
    const target = zones[nextIndex];
    target.querySelector<HTMLElement>(FOCUSABLE)?.focus();
    if (!target.contains(document.activeElement)) target.focus();
  }, []);

  const shortcutActions = useMemo(() => ({
    cycleFocus,
    openNewWindow: onOpenNewWindow,
    openProject: onOpenProject,
    toggleBottomPanel: () => update({ bottomPanelOpen: !layout.bottomPanelOpen }),
    toggleCommandPalette: () => setCommandPaletteOpen(true),
    toggleInspector,
    toggleSidebar,
  }), [cycleFocus, layout.bottomPanelOpen, onOpenNewWindow, onOpenProject, toggleInspector, toggleSidebar, update]);

  useGlobalShortcuts(shortcutActions, {
    bindings: shortcutSettings.bindings,
    enabled: !commandPaletteOpen && !shortcutSettingsOpen,
    platform,
  });

  const commandActions = useMemo<readonly CommandPaletteAction[]>(() => [
    { id: "open-project", label: "Open Project…", shortcut: shortcutLabel(shortcutSettings.bindings.openProject, platform), run: onOpenProject },
    { id: "new-window", label: "New Window", shortcut: shortcutLabel(shortcutSettings.bindings.openNewWindow, platform), run: onOpenNewWindow },
    { id: "toggle-sidebar", label: "Toggle Primary Sidebar", shortcut: shortcutLabel(shortcutSettings.bindings.toggleSidebar, platform), run: toggleSidebar },
    { id: "toggle-inspector", label: "Toggle Context Inspector", shortcut: shortcutLabel(shortcutSettings.bindings.toggleInspector, platform), run: toggleInspector },
    { id: "toggle-bottom", label: "Toggle Bottom Panel", shortcut: shortcutLabel(shortcutSettings.bindings.toggleBottomPanel, platform), run: () => update({ bottomPanelOpen: !layout.bottomPanelOpen }) },
    { id: "keyboard-shortcuts", label: "Keyboard Shortcuts…", run: () => setShortcutSettingsOpen(true) },
  ], [layout.bottomPanelOpen, onOpenNewWindow, onOpenProject, platform, shortcutSettings.bindings, toggleInspector, toggleSidebar, update]);

  const style = {
    "--bottom-size": `${layout.bottomHeight}px`,
    "--inspector-size": `${layout.inspectorWidth}px`,
    "--sidebar-size": `${layout.sidebarWidth}px`,
  } as CSSProperties;
  const drawerVisible = (responsive.sidebarDrawer && layout.sidebarOpen)
    || (responsive.inspectorDrawer && layout.inspectorOpen);

  return (
    <div
      className="project-window"
      data-bottom-open={layout.bottomPanelOpen}
      data-inspector-drawer={responsive.inspectorDrawer}
      data-inspector-open={layout.inspectorOpen}
      data-sidebar-drawer={responsive.sidebarDrawer}
      data-sidebar-open={layout.sidebarOpen}
      style={style}
    >
      <div className="project-window__content" aria-hidden={commandPaletteOpen || shortcutSettingsOpen || undefined} inert={commandPaletteOpen || shortcutSettingsOpen || undefined}>
        <div className="project-window__top">
          <ProjectToolbar
            appearance={appearance}
            inspectorOpen={layout.inspectorOpen}
            onCommandPalette={() => setCommandPaletteOpen(true)}
            onOpenProject={onOpenProject}
            onOpenNewWindow={onOpenNewWindow}
            onToggleBottom={() => update({ bottomPanelOpen: !layout.bottomPanelOpen })}
            onToggleInspector={toggleInspector}
            onToggleSidebar={toggleSidebar}
            openingProject={openingProject}
            platform={platform}
            projectName={project.name}
            sidebarOpen={layout.sidebarOpen}
          />
          {projectError ? (
            <div className="project-open-error" role="alert">
              <span>{projectError}</span>
              <button className="text-button" disabled={openingProject} onClick={onOpenProject} type="button">
                {openingProject ? "Opening…" : "Try another folder"}
              </button>
            </div>
          ) : null}
        </div>
        <div className="project-grid">
          <ActivityRail activity={layout.activity} onOpenSettings={() => setShortcutSettingsOpen(true)} onSelect={(activity) => update({ activity })} />
          <PrimarySidebar
            activity={layout.activity}
            drawer={responsive.sidebarDrawer}
            onClose={() => update({ sidebarOpen: false })}
            onOpenDiff={openDiff}
            onOpenFile={(path) => void openFile(path)}
            open={layout.sidebarOpen}
            project={project}
            projectClient={projectClient}
          />
          {!responsive.sidebarDrawer && layout.sidebarOpen ? (
            <PanelResizeHandle axis="horizontal" label="Resize primary sidebar" maximum={420} minimum={200} onChange={(sidebarWidth) => update({ sidebarWidth })} value={layout.sidebarWidth} />
          ) : null}
          <Workspace
            activeSurface={layout.workspaceSurface}
            fakeSession={fakeSession}
            onSelectSurface={(workspaceSurface) => update({ workspaceSurface })}
            diff={diff}
            draft={draft}
            file={file}
            loading={resourceLoading}
            onChangeDraft={setDraft}
            onOpenCompatibilityTui={openCompatibilityTui}
            onOpenFileExternal={() => void openFileExternal()}
            onSave={() => void saveFile()}
            projectName={project.name}
            resourceError={resourceError}
            saving={saving}
          />
          {!responsive.inspectorDrawer && layout.inspectorOpen ? (
            <PanelResizeHandle axis="horizontal" label="Resize context inspector" maximum={440} minimum={240} onChange={(inspectorWidth) => update({ inspectorWidth })} reverse value={layout.inspectorWidth} />
          ) : null}
          <ContextInspector drawer={responsive.inspectorDrawer} onClose={() => update({ inspectorOpen: false })} open={layout.inspectorOpen} />
          {drawerVisible ? <button className="drawer-scrim" aria-label="Close open side panel" onClick={() => update({ inspectorOpen: false, sidebarOpen: false })} type="button" /> : null}
        </div>
        <BottomPanel
          activeSurface={layout.bottomSurface}
          onClose={() => update({ bottomPanelOpen: false })}
          onSelectSurface={(bottomSurface) => update({ bottomSurface })}
          open={layout.bottomPanelOpen}
          daemon={daemon}
          fakeSession={fakeSession}
          fakeTui={fakeTui}
          shellTerminal={shellTerminal}
        />
        {layout.bottomPanelOpen ? (
          <PanelResizeHandle axis="vertical" label="Resize bottom panel" maximum={600} minimum={120} onChange={(bottomHeight) => update({ bottomHeight })} reverse value={layout.bottomHeight} />
        ) : null}
        <StatusBar daemon={daemon} />
      </div>
      {commandPaletteOpen ? <CommandPalette actions={commandActions} onClose={() => setCommandPaletteOpen(false)} /> : null}
      {shortcutSettingsOpen ? (
        <ShortcutSettingsDialog
          bindings={shortcutSettings.bindings}
          error={shortcutSettings.error}
          loading={shortcutSettings.loading}
          onClose={() => setShortcutSettingsOpen(false)}
          onSave={shortcutSettings.save}
        />
      ) : null}
    </div>
  );
}
