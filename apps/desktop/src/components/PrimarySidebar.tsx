import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import type { Activity } from "../hooks/useWindowLayout";
import {
  tauriProjectClient,
  type BranchState,
  type DirectoryEntry,
  type GitDiff,
  type GitStatusEntry,
  type ProjectClient,
  type SearchMatch,
  type Worktree,
} from "../lib/project";
import type { ProjectSelection } from "../lib/system";
import { Icon } from "./Icon";
import { WindowedList } from "./WindowedList";

interface PrimarySidebarProps {
  activity: Activity;
  drawer?: boolean;
  onClose: () => void;
  onOpenDiff: (diff: GitDiff) => void;
  onOpenFile: (path: string) => void;
  open: boolean;
  project: ProjectSelection;
  projectClient?: ProjectClient;
}

const content = {
  files: { title: "Files" },
  git: { title: "Source Control" },
  search: { title: "Search" },
  sessions: { title: "Sessions" },
} as const;

export function PrimarySidebar({
  activity,
  drawer = false,
  onClose,
  onOpenDiff,
  onOpenFile,
  open,
  project,
  projectClient = tauriProjectClient,
}: PrimarySidebarProps) {
  const selected = content[activity];
  return (
    <aside
      aria-label={selected.title + " sidebar"}
      className={"primary-sidebar " + (drawer ? "panel-drawer panel-drawer--left" : "")}
      data-open={open}
      data-focus-zone
      hidden={!open}
      tabIndex={-1}
    >
      <div className="panel-heading">
        <span>{selected.title}</span>
        {drawer ? (
          <button
            className="icon-button icon-button--small"
            onClick={onClose}
            type="button"
            aria-label={"Close " + selected.title.toLowerCase() + " sidebar"}
          >
            <Icon name="x" />
          </button>
        ) : null}
      </div>
      {activity === "files" ? (
        <FilesPanel client={projectClient} onOpenFile={onOpenFile} project={project} />
      ) : null}
      {activity === "search" ? (
        <SearchPanel client={projectClient} onOpenFile={onOpenFile} project={project} />
      ) : null}
      {activity === "git" ? (
        <GitPanel client={projectClient} onOpenDiff={onOpenDiff} project={project} />
      ) : null}
      {activity === "sessions" ? <SessionsPlaceholder /> : null}
    </aside>
  );
}

function FilesPanel({
  client,
  onOpenFile,
  project,
}: {
  client: ProjectClient;
  onOpenFile: (path: string) => void;
  project: ProjectSelection;
}) {
  const [root, setRoot] = useState(project.roots[0] ?? "");
  const [directory, setDirectory] = useState(root);
  const [entries, setEntries] = useState<DirectoryEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const loadGeneration = useRef(0);

  const load = useCallback(async (path: string) => {
    if (!path) return;
    const generation = ++loadGeneration.current;
    setLoading(true);
    setError(null);
    try {
      const page = await client.listDirectory(project.id, path);
      if (loadGeneration.current !== generation) return;
      setDirectory(page.directory);
      setEntries(page.entries);
    } catch {
      if (loadGeneration.current !== generation) return;
      setError("This folder could not be read safely.");
    } finally {
      if (loadGeneration.current === generation) setLoading(false);
    }
  }, [client, project.id]);

  const selectRoot = useCallback((nextRoot: string) => {
    loadGeneration.current += 1;
    setRoot(nextRoot);
    setDirectory(nextRoot);
    setEntries([]);
    setError(null);
    setLoading(false);
  }, []);

  useEffect(() => {
    if (!project.roots.includes(root)) {
      selectRoot(project.roots[0] ?? "");
      return;
    }
    setDirectory(root);
    void load(root);
  }, [load, project.roots, root, selectRoot]);

  useEffect(() => () => {
    loadGeneration.current += 1;
  }, []);

  const parent = directory !== root && directory.startsWith(root + "/")
    ? directory.slice(0, directory.lastIndexOf("/")) || root
    : null;
  return (
    <div className="project-browser">
      <div className="project-browser__toolbar">
        <button disabled={!parent} onClick={() => parent && void load(parent)} type="button">
          <span aria-hidden="true">←</span> Up
        </button>
        <button disabled={loading || !directory} onClick={() => void load(directory)} type="button">
          Refresh
        </button>
      </div>
      {project.roots.length > 1 ? (
        <label className="workspace-root-picker">
          <span>Workspace folder</span>
          <select
            aria-label="Workspace folder"
            onChange={(event) => selectRoot(event.target.value)}
            value={root}
          >
            {project.roots.map((workspaceRoot) => (
              <option key={workspaceRoot} value={workspaceRoot}>{basename(workspaceRoot)}</option>
            ))}
          </select>
        </label>
      ) : null}
      <p className="project-browser__path" title={directory}>{directory || "No workspace root"}</p>
      {error ? <p className="inline-error" role="alert">{error}</p> : null}
      {loading ? <p className="sidebar-status">Loading…</p> : null}
      {!loading && entries.length === 0 ? <p className="sidebar-status">This folder is empty.</p> : null}
      <WindowedList
        aria-label="Project files"
        as="ul"
        className="resource-list"
        estimatedRowHeight={32}
        itemKey={directoryEntryKey}
        items={entries}
        renderItem={(entry) => (
          <button
            onClick={() => {
              if (entry.kind === "directory") void load(entry.path);
              if (entry.kind === "file") onOpenFile(entry.path);
            }}
            disabled={entry.kind === "symlink" || entry.kind === "other"}
            title={entry.path}
            type="button"
          >
            <Icon name={entry.kind === "directory" ? "archive" : "files"} />
            <span>{entry.displayName}</span>
            {entry.kind === "symlink" ? <small>link</small> : null}
          </button>
        )}
      />
    </div>
  );
}

const directoryEntryKey = (entry: DirectoryEntry) => [entry.kind, entry.path].join(":");

function SearchPanel({
  client,
  onOpenFile,
  project,
}: {
  client: ProjectClient;
  onOpenFile: (path: string) => void;
  project: ProjectSelection;
}) {
  const [pattern, setPattern] = useState("");
  const [matches, setMatches] = useState<SearchMatch[]>([]);
  const [mode, setMode] = useState<"literal" | "regex">("literal");
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const activeSearch = useRef<string | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!pattern.trim() || running) return;
    const searchId = crypto.randomUUID();
    activeSearch.current = searchId;
    setRunning(true);
    setError(null);
    try {
      const result = await client.search(project.id, searchId, {
        pattern,
        mode,
        caseSensitive: false,
        includeHidden: false,
        maximumResults: 500,
        maximumFileBytes: 4 * 1024 * 1024,
      });
      setMatches(result.matches);
    } catch {
      setError("Search could not be completed.");
    } finally {
      if (activeSearch.current === searchId) activeSearch.current = null;
      setRunning(false);
    }
  };

  return (
    <div className="project-browser">
      <form className="search-field" onSubmit={(event) => void submit(event)}>
        <Icon name="search" />
        <input
          aria-label="Search project"
          onChange={(event) => setPattern(event.target.value)}
          placeholder="Search files"
          value={pattern}
        />
      </form>
      <label className="search-option">
        <span>Search syntax</span>
        <select
          aria-label="Search syntax"
          disabled={running}
          onChange={(event) => setMode(event.target.value as "literal" | "regex")}
          value={mode}
        >
          <option value="literal">Literal text</option>
          <option value="regex">Regular expression</option>
        </select>
      </label>
      <div className="project-browser__toolbar">
        <button disabled={!pattern.trim() || running} type="submit" onClick={(event) => void submit(event)}>
          Search
        </button>
        <button
          disabled={!running || !activeSearch.current}
          onClick={() => {
            const searchId = activeSearch.current;
            if (searchId) void client.cancelSearch(project.id, searchId);
          }}
          type="button"
        >
          Cancel
        </button>
      </div>
      {error ? <p className="inline-error" role="alert">{error}</p> : null}
      {running ? <p className="sidebar-status">Searching without indexing…</p> : null}
      <ul className="resource-list search-results" aria-label="Search results">
        {matches.map((match, index) => (
          <li key={[match.path, match.line, match.byteColumn, index].join(":")}>
            <button onClick={() => onOpenFile(match.path)} title={match.path} type="button">
              <Icon name="search" />
              <span>
                <strong>{basename(match.path)}:{match.line}</strong>
                <small>{match.excerpt}</small>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

function GitPanel({
  client,
  onOpenDiff,
  project,
}: {
  client: ProjectClient;
  onOpenDiff: (diff: GitDiff) => void;
  project: ProjectSelection;
}) {
  const [repository, setRepository] = useState(project.roots[0] ?? "");
  const [status, setStatus] = useState<GitStatusEntry[]>([]);
  const [branch, setBranch] = useState<BranchState | null>(null);
  const [worktrees, setWorktrees] = useState<Worktree[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [diffError, setDiffError] = useState<string | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);
  const refreshGeneration = useRef(0);
  const diffGeneration = useRef(0);

  const refresh = useCallback(async () => {
    if (!repository) return;
    const generation = ++refreshGeneration.current;
    setLoading(true);
    setError(null);
    try {
      const [nextStatus, nextBranch, nextWorktrees] = await Promise.all([
        client.gitStatus(project.id, repository),
        client.gitBranch(project.id, repository),
        client.gitWorktrees(project.id, repository),
      ]);
      if (refreshGeneration.current !== generation) return;
      setStatus(nextStatus);
      setBranch(nextBranch);
      setWorktrees(nextWorktrees);
    } catch {
      if (refreshGeneration.current !== generation) return;
      setError("Git information is unavailable for this folder.");
    } finally {
      if (refreshGeneration.current === generation) setLoading(false);
    }
  }, [client, project.id, repository]);

  const openDiff = useCallback(async () => {
    if (!repository) return;
    const generation = ++diffGeneration.current;
    setDiffLoading(true);
    setDiffError(null);
    try {
      const nextDiff = await client.gitDiff(project.id, repository, "working_tree");
      if (diffGeneration.current === generation) onOpenDiff(nextDiff);
    } catch {
      if (diffGeneration.current === generation) {
        setDiffError("The working tree diff could not be loaded.");
      }
    } finally {
      if (diffGeneration.current === generation) setDiffLoading(false);
    }
  }, [client, onOpenDiff, project.id, repository]);

  const selectRepository = useCallback((nextRepository: string) => {
    refreshGeneration.current += 1;
    diffGeneration.current += 1;
    setRepository(nextRepository);
    setStatus([]);
    setBranch(null);
    setWorktrees([]);
    setError(null);
    setDiffError(null);
    setLoading(false);
    setDiffLoading(false);
  }, []);

  useEffect(() => {
    if (!project.roots.includes(repository)) {
      selectRepository(project.roots[0] ?? "");
      return;
    }
    void refresh();
  }, [project.roots, refresh, repository, selectRepository]);

  useEffect(() => () => {
    refreshGeneration.current += 1;
    diffGeneration.current += 1;
  }, []);

  return (
    <div className="project-browser">
      <div className="project-browser__toolbar">
        <button disabled={loading} onClick={() => void refresh()} type="button">Refresh</button>
        <button
          disabled={loading || diffLoading || !repository}
          onClick={() => void openDiff()}
          type="button"
        >
          {diffLoading ? "Loading diff…" : "View diff"}
        </button>
      </div>
      {project.roots.length > 1 ? (
        <label className="workspace-root-picker">
          <span>Repository folder</span>
          <select
            aria-label="Repository folder"
            onChange={(event) => selectRepository(event.target.value)}
            value={repository}
          >
            {project.roots.map((workspaceRoot) => (
              <option key={workspaceRoot} value={workspaceRoot}>{basename(workspaceRoot)}</option>
            ))}
          </select>
        </label>
      ) : null}
      {branch ? <p className="project-browser__path">Branch: {branchLabel(branch)}</p> : null}
      {error ? <p className="inline-error" role="alert">{error}</p> : null}
      {diffError ? <p className="inline-error" role="alert">{diffError}</p> : null}
      {loading ? <p className="sidebar-status">Loading Git information…</p> : null}
      <ul className="resource-list" aria-label="Git changes">
        {status.map((entry, index) => (
          <li key={[entry.path.display, index].join(":")}>
            <button
              disabled={diffLoading}
              onClick={() => void openDiff()}
              title={entry.path.display}
              type="button"
            >
              <Icon name="branch" />
              <span>{entry.path.display}</span>
              <small>{entry.indexStatus}{entry.worktreeStatus}</small>
            </button>
          </li>
        ))}
      </ul>
      <section className="worktree-section" aria-labelledby="worktree-heading">
        <h2 id="worktree-heading">
          Worktrees <span className="count-badge">{worktrees.length}</span>
        </h2>
        {worktrees.length === 0 ? <p className="sidebar-status">No existing worktrees discovered.</p> : null}
        <ul className="worktree-list">
          {worktrees.map((worktree) => (
            <li key={worktree.path}>
              <p className="worktree-list__path" title={worktree.path}>{worktree.path}</p>
              <dl>
                <div>
                  <dt>State</dt>
                  <dd>{worktreeState(worktree)}</dd>
                </div>
                {worktree.head ? (
                  <div>
                    <dt>HEAD</dt>
                    <dd>{worktree.head}</dd>
                  </div>
                ) : null}
                {worktree.lockedReason ? (
                  <div>
                    <dt>Locked</dt>
                    <dd>{worktree.lockedReason}</dd>
                  </div>
                ) : null}
                {worktree.prunableReason ? (
                  <div>
                    <dt>Prunable</dt>
                    <dd>{worktree.prunableReason}</dd>
                  </div>
                ) : null}
              </dl>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}

function SessionsPlaceholder() {
  return (
    <>
      <div className="sidebar-empty">
        <Icon name="agents" />
        <p>Fake structured sessions are being connected to the Foundation event model.</p>
        <button aria-describedby="sessions-milestone-note" className="text-button" disabled type="button">
          + New session
        </button>
      </div>
      <div className="milestone-note" id="sessions-milestone-note" tabIndex={0}>
        <Icon name="info" />
        <span>Real agents become available with the Codex milestone.</span>
      </div>
    </>
  );
}

function basename(path: string) {
  return path.slice(path.lastIndexOf("/") + 1);
}

function branchLabel(branch: BranchState) {
  if (branch.state === "detached") return "detached " + branch.data.commit;
  return branch.data;
}

function worktreeState(worktree: Worktree) {
  if (worktree.bare) return "Bare repository";
  if (worktree.detached) return "Detached HEAD";
  return worktree.branch ? `Branch ${worktree.branch}` : "Branch unavailable";
}
