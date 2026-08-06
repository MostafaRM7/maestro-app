import type { AppearanceState } from "../hooks/useAppearance";
import { AppearanceControls } from "./AppearanceControls";
import { Icon } from "./Icon";

interface ProjectToolbarProps {
  appearance: AppearanceState;
  inspectorOpen: boolean;
  openingProject?: boolean;
  onCommandPalette: () => void;
  onOpenProject: () => void;
  onOpenNewWindow: () => void;
  onToggleBottom: () => void;
  onToggleInspector: () => void;
  onToggleSidebar: () => void;
  platform: string;
  projectName: string;
  sidebarOpen: boolean;
}

export function ProjectToolbar({
  appearance,
  inspectorOpen,
  openingProject = false,
  onCommandPalette,
  onOpenProject,
  onOpenNewWindow,
  onToggleBottom,
  onToggleInspector,
  onToggleSidebar,
  platform,
  projectName,
  sidebarOpen,
}: ProjectToolbarProps) {
  return (
    <header className="project-toolbar" data-focus-zone tabIndex={-1}>
      <button className="project-switcher" disabled={openingProject} onClick={onOpenProject} type="button" aria-label={`Switch project, current project ${projectName}`}>
        <span className="project-avatar" aria-hidden="true">M</span>
        <span>{projectName}</span>
        <Icon name="chevronDown" />
      </button>

      <div className="session-config" aria-label="Pending session configuration">
        <button aria-describedby="adapter-controls-help" className="select-button" disabled type="button">
          <span className="vendor-dot" aria-hidden="true" /> Codex <Icon name="chevronDown" />
        </button>
        <button aria-describedby="adapter-controls-help" className="select-button" disabled type="button">
          Model <Icon name="chevronDown" />
        </button>
        <button aria-describedby="adapter-controls-help" className="select-button session-config__optional" disabled type="button">
          Mode <Icon name="chevronDown" />
        </button>
        <button aria-describedby="adapter-controls-help" className="button button--primary" disabled type="button">
          <Icon name="plus" /> New Session
        </button>
        <span className="availability-note" id="adapter-controls-help" tabIndex={0}>Agent controls are available with the Codex milestone.</span>
      </div>

      <div className="toolbar-actions">
        <AppearanceControls {...appearance} />
        <button className="icon-button" type="button" onClick={onOpenNewWindow} aria-label="Open new window" title="Open new window">
          <Icon name="plus" />
        </button>
        <button className={`icon-button ${sidebarOpen ? "is-active" : ""}`} type="button" onClick={onToggleSidebar} aria-label="Toggle primary sidebar" aria-pressed={sidebarOpen} title="Toggle primary sidebar">
          <Icon name="panelLeft" />
        </button>
        <button className={`icon-button ${inspectorOpen ? "is-active" : ""}`} type="button" onClick={onToggleInspector} aria-label="Toggle context inspector" aria-pressed={inspectorOpen} title="Toggle context inspector">
          <Icon name="panelRight" />
        </button>
        <button className="icon-button" type="button" onClick={onToggleBottom} aria-label="Toggle bottom panel" title="Toggle bottom panel">
          <Icon name="panelBottom" />
        </button>
        <button className="command-button" type="button" onClick={onCommandPalette}>
          <Icon name="command" />
          <span>Commands</span>
          <kbd>{platform === "macos" ? "⇧⌘P" : "Ctrl+Shift+P"}</kbd>
        </button>
      </div>
    </header>
  );
}
