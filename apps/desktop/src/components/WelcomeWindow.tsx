import type { DaemonSnapshot, RecentProject, SystemSnapshot } from "../lib/system";
import { Icon } from "./Icon";

interface WelcomeWindowProps {
  daemon: DaemonSnapshot;
  error: string | null;
  favoriteProjectId: string | null;
  onOpenProject: () => void;
  onOpenNewWindow: () => void;
  onOpenRecentProject: (projectId: string) => void;
  onSetFavorite: (projectId: string, favorite: boolean) => void;
  openingProject: boolean;
  openingRecentProjectId: string | null;
  recentProjects: readonly RecentProject[];
  recentProjectsLoading: boolean;
  snapshot: SystemSnapshot;
}

export function WelcomeWindow({
  daemon,
  error,
  favoriteProjectId,
  onOpenProject,
  onOpenNewWindow,
  onOpenRecentProject,
  onSetFavorite,
  openingProject,
  openingRecentProjectId,
  recentProjects,
  recentProjectsLoading,
  snapshot,
}: WelcomeWindowProps) {
  const daemonConnected = daemon.status === "connected";

  return (
    <main className="welcome-window" data-focus-zone tabIndex={-1}>
      <div className="welcome-brand" aria-hidden="true"><Icon name="spark" /></div>
      <p className="eyebrow">Agent control center</p>
      <h1>Welcome to Maestro</h1>
      <p className="welcome-window__lede">Open a project to start supervising agents.</p>
      {!daemonConnected ? (
        <div className="service-banner" role="status">
          <Icon name="warning" />
          <span><strong>Agent service offline.</strong> You can open the local project shell, but sessions and terminals remain unavailable.</span>
        </div>
      ) : null}
      {error ? <div className="inline-error" role="alert">{error}</div> : null}
      <button className="button button--primary" disabled={openingProject} type="button" onClick={onOpenProject} autoFocus>
        <Icon name="files" />
        {openingProject ? "Opening Project…" : "Open Project…"}
      </button>
      <button className="button" disabled={openingProject} type="button" onClick={onOpenNewWindow}>
        <Icon name="plus" /> New Window
      </button>
      <section className="welcome-card" aria-labelledby="recent-projects-title">
        <div className="welcome-card__header">
          <h2 id="recent-projects-title">Recent projects</h2>
          <span className={`status-chip ${daemonConnected ? "status-chip--success" : "status-chip--warning"}`}>
            {daemonConnected ? "Service connected" : "Service offline"}
          </span>
        </div>
        {recentProjectsLoading ? <p role="status">Loading recent projects…</p> : null}
        {!recentProjectsLoading && recentProjects.length === 0 ? <p>No projects have been opened yet.</p> : null}
        {recentProjects.length > 0 ? (
          <ul className="recent-projects">
            {recentProjects.map((recentProject) => {
              const opening = openingRecentProjectId === recentProject.projectId;
              const changingFavorite = favoriteProjectId === recentProject.projectId;
              return (
                <li key={recentProject.projectId}>
                  <button
                    className="recent-project__open"
                    disabled={openingProject}
                    onClick={() => onOpenRecentProject(recentProject.projectId)}
                    type="button"
                  >
                    <strong>{opening ? `Opening ${recentProject.displayName}…` : recentProject.displayName}</strong>
                    <span>{recentProject.canonicalRoots[0] ?? "Saved workspace"}</span>
                  </button>
                  <button
                    aria-label={`${recentProject.favorite ? "Remove" : "Add"} ${recentProject.displayName} ${recentProject.favorite ? "from" : "to"} favorites`}
                    aria-pressed={recentProject.favorite}
                    className="recent-project__favorite"
                    disabled={changingFavorite}
                    onClick={() => onSetFavorite(recentProject.projectId, !recentProject.favorite)}
                    title={recentProject.favorite ? "Remove from favorites" : "Add to favorites"}
                    type="button"
                  >
                    <span aria-hidden="true">{recentProject.favorite ? "★" : "☆"}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        ) : null}
      </section>
      <p className="host-caption">{snapshot.platform} · {snapshot.architecture} · v{snapshot.appVersion}</p>
    </main>
  );
}
