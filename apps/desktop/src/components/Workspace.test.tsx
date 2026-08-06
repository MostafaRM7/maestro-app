import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { FakeSessionController } from "../hooks/useFakeSession";
import type { GitDiff } from "../lib/project";
import { Workspace } from "./Workspace";

const unifiedDiff = [
  "diff --git a/src/value.ts b/src/value.ts",
  "index 1111111..2222222 100644",
  "--- a/src/value.ts",
  "+++ b/src/value.ts",
  "@@ -1,3 +1,3 @@",
  " export const unchanged = true;",
  "-const oldValue = 1;",
  "+const newValue = 2;",
  " export default unchanged;",
  "",
].join("\n");

function renderDiff(diff: GitDiff) {
  return render(
    <Workspace
      activeSurface="conversation"
      diff={diff}
      draft=""
      fakeSession={{} as FakeSessionController}
      file={null}
      loading={false}
      onChangeDraft={vi.fn()}
      onOpenCompatibilityTui={vi.fn()}
      onOpenFileExternal={vi.fn()}
      onSave={vi.fn()}
      onSelectSurface={vi.fn()}
      projectName="Maestro"
      resourceError={null}
      saving={false}
    />,
  );
}

describe("Workspace diff review", () => {
  it("parses and renders unified diffs in inline and side-by-side modes", async () => {
    const user = userEvent.setup();
    renderDiff({ containsBinaryChanges: false, text: unifiedDiff, truncated: false });

    const inline = screen.getByRole("table", { name: "Git diff, inline view" });
    expect(within(inline).getByText("-const oldValue = 1;")).toBeInTheDocument();
    expect(within(inline).getByText("+const newValue = 2;")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Inline" })).toHaveAttribute("aria-pressed", "true");

    await user.click(screen.getByRole("button", { name: "Side by side" }));

    const split = screen.getByRole("table", { name: "Git diff, side-by-side view" });
    expect(within(split).getByText("-const oldValue = 1;")).toBeInTheDocument();
    expect(within(split).getByText("+const newValue = 2;")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Side by side" })).toHaveAttribute("aria-pressed", "true");
  });

  it("renders diff content as text rather than executable markup", () => {
    const hostileText = [
      "diff --git a/message.txt b/message.txt",
      "--- a/message.txt",
      "+++ b/message.txt",
      "@@ -0,0 +1 @@",
      "+<img src=x onerror=window.__diffInjected=true>",
    ].join("\n");
    const { container } = renderDiff({ containsBinaryChanges: false, text: hostileText, truncated: false });

    expect(screen.getByText("+<img src=x onerror=window.__diffInjected=true>")).toBeInTheDocument();
    expect(container.querySelector("img")).toBeNull();
  });

  it("communicates binary and truncated diff limitations independently", () => {
    renderDiff({ containsBinaryChanges: true, text: "", truncated: true });

    expect(screen.getByText("Truncated")).toBeInTheDocument();
    expect(screen.getByText("Includes binary changes")).toBeInTheDocument();
    expect(screen.getByText("This diff was truncated at the configured size limit.")).toBeInTheDocument();
    expect(screen.getByText("Binary changes are listed, but their contents cannot be rendered as text.")).toBeInTheDocument();
    expect(screen.getByText("No textual diff is available for these binary changes.")).toBeInTheDocument();
  });

  it("propagates incompatible-session fallback actions to the project window", async () => {
    const onOpenCompatibilityTui = vi.fn();
    const user = userEvent.setup();
    const fakeSession = {
      attach: vi.fn().mockResolvedValue(undefined),
      error: null,
      events: [],
      launching: false,
      listError: null,
      listLoading: false,
      pendingInput: null,
      pendingPermission: null,
      rawCaptureEnabled: false,
      rawCaptureError: null,
      rawProtocol: null,
      replayGap: false,
      reloadSessions: vi.fn().mockResolvedValue(undefined),
      resolvedRequestIds: new Set(),
      respondPermission: vi.fn().mockResolvedValue(true),
      respondUserInput: vi.fn().mockResolvedValue(true),
      resume: vi.fn().mockResolvedValue(undefined),
      run: { processId: 42, runId: "run-1", sessionId: "session-1" },
      sendDemoGuiAction: vi.fn().mockResolvedValue(undefined),
      sessions: [],
      setRawCaptureEnabled: vi.fn(),
      snapshot: {
        activeRunId: null,
        binding: null,
        droppedThroughSequence: 0,
        lastError: null,
        lastExit: null,
        latestSequence: 1,
        sessionId: "session-1",
        state: "incompatible",
        stderr: "",
        stderrTruncated: false,
      },
      start: vi.fn().mockResolvedValue(undefined),
      state: "incompatible",
      stop: vi.fn().mockResolvedValue(undefined),
      stopping: false,
    } satisfies FakeSessionController;
    render(
      <Workspace
        activeSurface="conversation"
        diff={null}
        draft=""
        fakeSession={fakeSession}
        file={null}
        loading={false}
        onChangeDraft={vi.fn()}
        onOpenCompatibilityTui={onOpenCompatibilityTui}
        onOpenFileExternal={vi.fn()}
        onSave={vi.fn()}
        onSelectSurface={vi.fn()}
        projectName="Maestro"
        resourceError={null}
        saving={false}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Open Exact TUI Compatibility Mode" }));
    expect(onOpenCompatibilityTui).toHaveBeenCalledOnce();
  });
});
