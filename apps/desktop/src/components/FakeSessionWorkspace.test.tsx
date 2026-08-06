import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type {
  FakeSessionController,
  PendingSessionRequest,
} from "../hooks/useFakeSession";
import type { EventEnvelope, SessionSnapshot, SessionState } from "../lib/fakeSession";
import { FakeSessionWorkspace } from "./FakeSessionWorkspace";

const permission: PendingSessionRequest = {
  eventId: "event-permission",
  payload: {
    command: ["git", "status"],
    paths: ["."],
    request_id: "permission-1",
  },
  requestId: "permission-1",
  runId: "run-1",
};

const input: PendingSessionRequest = {
  eventId: "event-input",
  payload: {
    choices: ["alpha", "beta"],
    prompt: "Choose a deterministic answer",
    request_id: "input-1",
  },
  requestId: "input-1",
  runId: "run-1",
};

function controller(overrides: Partial<FakeSessionController> = {}): FakeSessionController {
  return {
    attach: vi.fn().mockResolvedValue(undefined),
    error: null,
    events: [],
    launching: false,
    listError: null,
    listLoading: false,
    pendingInput: null,
    pendingPermission: null,
    replayGap: false,
    rawCaptureEnabled: false,
    rawCaptureError: null,
    rawProtocol: null,
    resolvedRequestIds: new Set(),
    reloadSessions: vi.fn().mockResolvedValue(undefined),
    run: {
      processId: 42,
      runId: "run-1",
      sessionId: "session-1",
    },
    snapshot: null,
    sessions: [],
    state: "running",
    stopping: false,
    setRawCaptureEnabled: vi.fn(),
    resume: vi.fn().mockResolvedValue(undefined),
    respondPermission: vi.fn().mockResolvedValue(true),
    respondUserInput: vi.fn().mockResolvedValue(true),
    sendDemoGuiAction: vi.fn().mockResolvedValue(undefined),
    start: vi.fn().mockResolvedValue(undefined),
    stop: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

function settledSnapshot(
  state: SessionState,
  overrides: Partial<SessionSnapshot> = {},
): SessionSnapshot {
  return {
    activeRunId: null,
    binding: null,
    droppedThroughSequence: 0,
    lastError: null,
    lastExit: null,
    latestSequence: 12,
    sessionId: "session-1",
    state,
    stderr: "",
    stderrTruncated: false,
    ...overrides,
  };
}

function eventEnvelope(index: number): EventEnvelope {
  return {
    event_id: `event-${index}`,
    event: {
      kind: "assistant_message",
      payload: { text: `Event ${index}` },
      raw_segment_reference: null,
      vendor_event_id: null,
      visibility: "user",
    },
    run_id: "run-1",
    sequence: index,
    session_id: "session-1",
    source: "cli",
    timestamp: "2026-08-05T10:00:00Z",
  };
}

function renderWorkspace(
  fake: FakeSessionController,
  onOpenCompatibilityTui = vi.fn(),
) {
  return {
    onOpenCompatibilityTui,
    ...render(
      <FakeSessionWorkspace
        onOpenCompatibilityTui={onOpenCompatibilityTui}
        projectName="Test project"
        session={fake}
      />,
    ),
  };
}

describe("fake structured-session workspace", () => {
  it("windows large rich event histories while preserving an accessible scroll target", () => {
    renderWorkspace(controller({
      events: Array.from({ length: 1_000 }, (_, index) => eventEnvelope(index)),
    }));

    const history = screen.getByRole("list", { name: "Normalized fake session event history" });
    expect(history).toHaveAttribute("tabindex", "0");
    expect(history.querySelectorAll("li").length).toBeLessThan(50);
  });

  it("clearly labels and launches only the deterministic fake harness", async () => {
    const fake = controller({ run: null, state: null });
    const user = userEvent.setup();
    renderWorkspace(fake);

    expect(screen.getByRole("heading", { name: "Run a deterministic fake structured session" })).toBeInTheDocument();
    expect(screen.getByText(/It is not Codex, Claude Code, or agy/)).toBeInTheDocument();
    const capture = screen.getByRole("checkbox", { name: /Capture exact raw protocol bytes/ });
    expect(capture).not.toBeChecked();
    await user.click(capture);
    expect(fake.setRawCaptureEnabled).toHaveBeenCalledWith(true);
    await user.selectOptions(screen.getByLabelText("Fixture scenario"), "structured/permission");
    await user.click(screen.getByRole("button", { name: "Start Fake Session" }));

    expect(fake.start).toHaveBeenCalledWith("structured/permission");
  });

  it.each([
    ["Allow once", "allow"],
    ["Deny", "deny"],
    ["Cancel request", "cancel"],
  ] as const)("sends %s as exactly one request-scoped permission decision", async (button, decision) => {
    const fake = controller({ pendingPermission: permission, state: "awaiting_permission" });
    const user = userEvent.setup();
    renderWorkspace(fake);

    await user.dblClick(screen.getByRole("button", { name: button }));

    expect(fake.respondPermission).toHaveBeenCalledOnce();
    expect(fake.respondPermission).toHaveBeenCalledWith(permission, decision);
    expect(screen.getByRole("button", { name: button })).toBeDisabled();
  });

  it("sends one sensitive user-input response and disables every response action", async () => {
    const fake = controller({ pendingInput: input, state: "awaiting_user_input" });
    const user = userEvent.setup();
    renderWorkspace(fake);

    await user.click(screen.getByRole("button", { name: "beta" }));
    await user.dblClick(screen.getByRole("button", { name: "Send once" }));

    expect(fake.respondUserInput).toHaveBeenCalledOnce();
    expect(fake.respondUserInput).toHaveBeenCalledWith(input, "beta");
    expect(screen.getByRole("button", { name: "Cancel input" })).toBeDisabled();
  });

  it("re-enables a permission request only when delivery fails, then permits a retry", async () => {
    const respondPermission = vi.fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const fake = controller({ pendingPermission: permission, respondPermission, state: "awaiting_permission" });
    const user = userEvent.setup();
    renderWorkspace(fake);

    const allow = screen.getByRole("button", { name: "Allow once" });
    await user.click(allow);
    await waitFor(() => expect(allow).toBeEnabled());
    await user.click(allow);

    expect(respondPermission).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(allow).toBeDisabled());
  });

  it("preserves sensitive input for a retry after non-delivery", async () => {
    const respondUserInput = vi.fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const fake = controller({ pendingInput: input, respondUserInput, state: "awaiting_user_input" });
    const user = userEvent.setup();
    renderWorkspace(fake);

    await user.type(screen.getByLabelText("Response"), "retry value");
    const send = screen.getByRole("button", { name: "Send once" });
    await user.click(send);
    await waitFor(() => expect(send).toBeEnabled());
    expect(screen.getByLabelText("Response")).toHaveValue("retry value");
    await user.click(send);

    expect(respondUserInput).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(send).toBeDisabled());
  });

  it("stops only from the explicit fake-session action", async () => {
    const fake = controller();
    const user = userEvent.setup();
    renderWorkspace(fake);

    expect(fake.stop).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Stop Fake Session" }));

    expect(fake.stop).toHaveBeenCalledOnce();
  });

  it("exposes correlated GUI actions only while the fixture is ready", async () => {
    const fake = controller({ state: "ready" });
    const user = userEvent.setup();
    renderWorkspace(fake);

    await user.click(screen.getByRole("button", { name: "Send session.inspect(…)" }));

    expect(fake.sendDemoGuiAction).toHaveBeenCalledOnce();
  });

  it("renders bounded non-zero diagnostics without exposing arbitrary exit objects", () => {
    const fake = controller({
      snapshot: settledSnapshot("failed", {
        lastError: {
          code: "process_crashed",
          correlationId: "11111111-2222-4333-8444-555555555555",
          message: "the fake CLI process exited unsuccessfully",
        },
        lastExit: { cause: "exited", value: 7 },
        stderr: "x".repeat(5_000),
      }),
      state: "failed",
    });
    renderWorkspace(fake);

    expect(screen.getByRole("heading", { name: "Session failed" })).toBeInTheDocument();
    expect(screen.getByText("Exited with code 7")).toBeInTheDocument();
    expect(screen.getByText("process_crashed")).toBeInTheDocument();
    expect(screen.getByText("the fake CLI process exited unsuccessfully")).toBeInTheDocument();
    expect(screen.getByText("11111111-2222-4333-8444-555555555555")).toBeInTheDocument();
    expect(screen.getByLabelText("Bounded session stderr").textContent).toHaveLength(4_096);
    expect(screen.getByText("This stderr preview was truncated by the UI display limit.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry in a New Fixture Run" })).toBeInTheDocument();
  });

  it("reports crash signals and daemon stderr truncation honestly", () => {
    const fake = controller({
      snapshot: settledSnapshot("failed", {
        lastExit: { cause: "signaled", value: 9 },
        stderr: "crash detail",
        stderrTruncated: true,
      }),
      state: "failed",
    });
    renderWorkspace(fake);

    expect(screen.getByText("Terminated by signal 9")).toBeInTheDocument();
    expect(screen.getByText("Earlier stderr was truncated by the daemon retention limit.")).toBeInTheDocument();
  });

  it("routes incompatible structured sessions to exact TUI compatibility mode", async () => {
    const fake = controller({
      snapshot: settledSnapshot("incompatible", {
        lastError: {
          code: "protocol_incompatible",
          correlationId: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
          message: "unsupported fixture protocol",
        },
      }),
      state: "incompatible",
    });
    const openCompatibility = vi.fn();
    const user = userEvent.setup();
    renderWorkspace(fake, openCompatibility);

    expect(screen.getByRole("heading", { name: "Structured integration is incompatible" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open Exact TUI Compatibility Mode" }));
    expect(openCompatibility).toHaveBeenCalledOnce();
    expect(fake.resume).not.toHaveBeenCalled();
  });

  it("explains interrupted limitations and offers only supported recovery", async () => {
    const fake = controller({
      snapshot: settledSnapshot("interrupted"),
      state: "interrupted",
    });
    const user = userEvent.setup();
    renderWorkspace(fake);

    expect(screen.getByText(/Exact continuation is unavailable/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Attempt Supported Recovery" }));
    expect(fake.resume).toHaveBeenCalledOnce();
  });

  it("renders the recoverable outcome and its supported recovery action", async () => {
    const fake = controller({ state: "recoverable" });
    const user = userEvent.setup();
    renderWorkspace(fake);

    expect(screen.getByRole("heading", { name: "Session is recoverable" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Stop Fake Session" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Attempt Supported Recovery" }));
    expect(fake.resume).toHaveBeenCalledOnce();
  });

  it.each([
    ["completed", "Session completed successfully", "Start a Follow-up Fixture Run"],
    ["stopped", "Session stopped by request", "Start a New Fixture Run"],
  ] as const)("uses honest %s outcome and action labels", (state, heading, action) => {
    const fake = controller({ snapshot: settledSnapshot(state), state });
    renderWorkspace(fake);

    expect(screen.getByRole("heading", { name: heading })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: action })).toBeInTheDocument();
  });
});
