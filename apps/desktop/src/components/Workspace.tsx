import { useMemo, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import type { WorkspaceSurface } from "../hooks/useWindowLayout";
import type { FakeSessionController } from "../hooks/useFakeSession";
import type { GitDiff, TextFile } from "../lib/project";
import { FakeSessionWorkspace } from "./FakeSessionWorkspace";
import { Icon } from "./Icon";

interface WorkspaceProps {
  activeSurface: WorkspaceSurface;
  diff: GitDiff | null;
  draft: string;
  file: TextFile | null;
  fakeSession: FakeSessionController;
  loading: boolean;
  onChangeDraft: (value: string) => void;
  onOpenCompatibilityTui: () => void;
  onOpenFileExternal: () => void;
  onSave: () => void;
  onSelectSurface: (surface: WorkspaceSurface) => void;
  projectName: string;
  resourceError: string | null;
  saving: boolean;
}

export function Workspace({
  activeSurface,
  diff,
  draft,
  file,
  fakeSession,
  loading,
  onChangeDraft,
  onOpenCompatibilityTui,
  onOpenFileExternal,
  onSave,
  onSelectSurface,
  projectName,
  resourceError,
  saving,
}: WorkspaceProps) {
  return (
    <main className="workspace" data-focus-zone tabIndex={-1}>
      <div className="workspace-tabs" role="tablist" aria-label="Workspace tabs">
        <button
          aria-selected={activeSurface === "conversation"}
          aria-controls="workspace-panel-conversation"
          className={activeSurface === "conversation" ? "is-active" : ""}
          onClick={() => onSelectSurface("conversation")}
          onKeyDown={(event) => handleTabKey(event, "conversation", onSelectSurface)}
          role="tab"
          id="workspace-tab-conversation"
          tabIndex={activeSurface === "conversation" ? 0 : -1}
          type="button"
        >
          <Icon name={file ? "files" : diff ? "branch" : "spark"} /> {file ? basename(file.path) : diff ? "Working tree diff" : "Foundation"} <span className="tab-close" aria-hidden="true">×</span>
        </button>
        <button
          aria-selected={activeSurface === "plan"}
          aria-controls="workspace-panel-plan"
          className={activeSurface === "plan" ? "is-active" : ""}
          onClick={() => onSelectSurface("plan")}
          onKeyDown={(event) => handleTabKey(event, "plan", onSelectSurface)}
          role="tab"
          id="workspace-tab-plan"
          tabIndex={activeSurface === "plan" ? 0 : -1}
          type="button"
        >
          <Icon name="archive" /> Plan
        </button>
      </div>
      <section aria-labelledby="workspace-tab-conversation" className="workspace-canvas" hidden={activeSurface !== "conversation"} id="workspace-panel-conversation" role="tabpanel">
        {resourceError ? <p className="workspace-error" role="alert">{resourceError}</p> : null}
        {loading ? <div className="workspace-loading">Loading project resource…</div> : null}
        {!loading && file ? (
          <FileReview
            draft={draft}
            file={file}
            onChange={onChangeDraft}
            onOpenExternal={onOpenFileExternal}
            onSave={onSave}
            saving={saving}
          />
        ) : null}
        {!loading && !file && diff ? <DiffReview diff={diff} /> : null}
        {!loading && !file && !diff ? (
          <FakeSessionWorkspace
            onOpenCompatibilityTui={onOpenCompatibilityTui}
            projectName={projectName}
            session={fakeSession}
          />
        ) : null}
      </section>
      <section aria-labelledby="workspace-tab-plan" className="workspace-canvas" hidden={activeSurface !== "plan"} id="workspace-panel-plan" role="tabpanel">
        <PlanPlaceholder />
      </section>
    </main>
  );
}

function FileReview({
  draft,
  file,
  onChange,
  onOpenExternal,
  onSave,
  saving,
}: {
  draft: string;
  file: TextFile;
  onChange: (value: string) => void;
  onOpenExternal: () => void;
  onSave: () => void;
  saving: boolean;
}) {
  const changed = draft !== file.text;
  return (
    <div className="file-review">
      <header>
        <div>
          <p className="eyebrow">Lightweight editor</p>
          <h1>{basename(file.path)}</h1>
          <p title={file.path}>{file.path}</p>
        </div>
        <div className="file-review__actions">
          <button className="button" onClick={onOpenExternal} type="button">
            Open Externally
          </button>
          <button
            className="button button--primary"
            disabled={!changed || saving}
            onClick={onSave}
            type="button"
          >
            {saving ? "Saving…" : changed ? "Save" : "Saved"}
          </button>
        </div>
      </header>
      <textarea
        aria-label={"Edit " + basename(file.path)}
        onChange={(event) => onChange(event.target.value)}
        spellCheck={false}
        value={draft}
      />
      <footer>{file.bytes.toLocaleString()} bytes · saves are atomic and conflict-checked</footer>
    </div>
  );
}

type DiffLayout = "inline" | "side-by-side";
type DiffLineKind = "addition" | "context" | "deletion" | "hunk" | "metadata";

interface ParsedDiffLine {
  kind: DiffLineKind;
  newLine: number | null;
  oldLine: number | null;
  text: string;
}

interface SideBySideRow {
  annotation: ParsedDiffLine | null;
  newLine: ParsedDiffLine | null;
  oldLine: ParsedDiffLine | null;
}

function DiffReview({ diff }: { diff: GitDiff }) {
  const [layout, setLayout] = useState<DiffLayout>("inline");
  const lines = useMemo(() => parseUnifiedDiff(diff.text), [diff.text]);
  const sideBySideRows = useMemo(() => toSideBySideRows(lines), [lines]);
  const hasRenderableText = lines.length > 0;

  return (
    <div className="diff-review">
      <header>
        <div>
          <p className="eyebrow">Git inspection</p>
          <h1>Working tree diff</h1>
        </div>
        <div className="diff-review__badges" aria-label="Diff status">
          <span className="support-badge">Read-only</span>
          {diff.truncated ? <span className="support-badge">Truncated</span> : null}
          {diff.containsBinaryChanges ? <span className="support-badge">Includes binary changes</span> : null}
        </div>
      </header>
      <div className="diff-review__toolbar" role="group" aria-label="Diff layout">
        <button aria-pressed={layout === "inline"} onClick={() => setLayout("inline")} type="button">
          Inline
        </button>
        <button
          aria-pressed={layout === "side-by-side"}
          onClick={() => setLayout("side-by-side")}
          type="button"
        >
          Side by side
        </button>
      </div>
      {diff.truncated ? (
        <p className="diff-review__notice" role="status">This diff was truncated at the configured size limit.</p>
      ) : null}
      {diff.containsBinaryChanges ? (
        <p className="diff-review__notice" role="status">Binary changes are listed, but their contents cannot be rendered as text.</p>
      ) : null}
      {!hasRenderableText ? (
        <p className="diff-review__empty">
          {diff.containsBinaryChanges ? "No textual diff is available for these binary changes." : "No working tree changes."}
        </p>
      ) : layout === "inline" ? (
        <InlineDiff lines={lines} />
      ) : (
        <SideBySideDiff rows={sideBySideRows} />
      )}
    </div>
  );
}

function InlineDiff({ lines }: { lines: ParsedDiffLine[] }) {
  return (
    <div className="diff-table-wrap">
      <table className="diff-table diff-table--inline" aria-label="Git diff, inline view">
        <thead className="visually-hidden">
          <tr><th>Old line</th><th>New line</th><th>Content</th></tr>
        </thead>
        <tbody>
          {lines.map((line, index) => (
            <tr className={`diff-line diff-line--${line.kind}`} key={index}>
              <td className="diff-line__number">{line.oldLine}</td>
              <td className="diff-line__number">{line.newLine}</td>
              <td><code>{line.text || " "}</code></td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SideBySideDiff({ rows }: { rows: SideBySideRow[] }) {
  return (
    <div className="diff-table-wrap">
      <table className="diff-table diff-table--split" aria-label="Git diff, side-by-side view">
        <thead className="visually-hidden">
          <tr><th>Old line</th><th>Old content</th><th>New line</th><th>New content</th></tr>
        </thead>
        <tbody>
          {rows.map((row, index) => row.annotation ? (
            <tr className={`diff-line diff-line--${row.annotation.kind}`} key={index}>
              <td colSpan={4}><code>{row.annotation.text || " "}</code></td>
            </tr>
          ) : (
            <tr className="diff-line" key={index}>
              <td className="diff-line__number diff-line--deletion">{row.oldLine?.oldLine}</td>
              <td className={row.oldLine?.kind === "deletion" ? "diff-line--deletion" : "diff-line--context"}>
                <code>{row.oldLine?.text ?? " "}</code>
              </td>
              <td className="diff-line__number diff-line--addition">{row.newLine?.newLine}</td>
              <td className={row.newLine?.kind === "addition" ? "diff-line--addition" : "diff-line--context"}>
                <code>{row.newLine?.text ?? " "}</code>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function parseUnifiedDiff(text: string): ParsedDiffLine[] {
  if (!text) return [];
  const rawLines = text.replaceAll("\r\n", "\n").replaceAll("\r", "\n").split("\n");
  if (rawLines[rawLines.length - 1] === "") rawLines.pop();

  const parsed: ParsedDiffLine[] = [];
  let oldLine = 0;
  let newLine = 0;
  let inHunk = false;
  for (const textLine of rawLines) {
    if (textLine.startsWith("diff --git ")) {
      inHunk = false;
      parsed.push({ kind: "metadata", newLine: null, oldLine: null, text: textLine });
      continue;
    }
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(textLine);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      inHunk = true;
      parsed.push({ kind: "hunk", newLine: null, oldLine: null, text: textLine });
      continue;
    }
    if (inHunk && textLine.startsWith("+")) {
      parsed.push({ kind: "addition", newLine, oldLine: null, text: textLine });
      newLine += 1;
      continue;
    }
    if (inHunk && textLine.startsWith("-")) {
      parsed.push({ kind: "deletion", newLine: null, oldLine, text: textLine });
      oldLine += 1;
      continue;
    }
    if (inHunk && textLine.startsWith(" ")) {
      parsed.push({ kind: "context", newLine, oldLine, text: textLine });
      oldLine += 1;
      newLine += 1;
      continue;
    }
    parsed.push({ kind: "metadata", newLine: null, oldLine: null, text: textLine });
  }
  return parsed;
}

function toSideBySideRows(lines: ParsedDiffLine[]): SideBySideRow[] {
  const rows: SideBySideRow[] = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (line.kind === "addition" || line.kind === "deletion") {
      const deletions: ParsedDiffLine[] = [];
      const additions: ParsedDiffLine[] = [];
      while (index < lines.length && (lines[index].kind === "addition" || lines[index].kind === "deletion")) {
        const changedLine = lines[index];
        if (changedLine.kind === "deletion") deletions.push(changedLine);
        if (changedLine.kind === "addition") additions.push(changedLine);
        index += 1;
      }
      const rowCount = Math.max(deletions.length, additions.length);
      for (let changeIndex = 0; changeIndex < rowCount; changeIndex += 1) {
        rows.push({
          annotation: null,
          newLine: additions[changeIndex] ?? null,
          oldLine: deletions[changeIndex] ?? null,
        });
      }
      continue;
    }
    if (line.kind === "context") {
      rows.push({ annotation: null, newLine: line, oldLine: line });
    } else {
      rows.push({ annotation: line, newLine: null, oldLine: null });
    }
    index += 1;
  }
  return rows;
}

const workspaceTabs: readonly WorkspaceSurface[] = ["conversation", "plan"];

function handleTabKey(event: ReactKeyboardEvent<HTMLButtonElement>, current: WorkspaceSurface, select: (surface: WorkspaceSurface) => void) {
  const currentIndex = workspaceTabs.indexOf(current);
  let nextIndex: number | null = null;
  if (event.key === "ArrowRight") nextIndex = (currentIndex + 1) % workspaceTabs.length;
  if (event.key === "ArrowLeft") nextIndex = (currentIndex - 1 + workspaceTabs.length) % workspaceTabs.length;
  if (event.key === "Home") nextIndex = 0;
  if (event.key === "End") nextIndex = workspaceTabs.length - 1;
  if (nextIndex === null) return;
  event.preventDefault();
  const next = workspaceTabs[nextIndex];
  select(next);
  document.getElementById(`workspace-tab-${next}`)?.focus();
}

function PlanPlaceholder() {
  return (
    <div className="plan-placeholder">
      <div className="empty-illustration" aria-hidden="true"><Icon name="archive" /></div>
      <p className="eyebrow">Plan</p>
      <h1>No plan is active</h1>
      <p>When a CLI publishes a plan, its steps and progress will be represented here.</p>
      <span className="support-badge">Available with Codex milestone</span>
    </div>
  );
}

function basename(path: string) {
  return path.slice(path.lastIndexOf("/") + 1);
}
