import { describe, expect, it, vi } from "vitest";
import {
  createTauriFakeSessionClient,
  formatRawProtocolBytes,
  mergeSessionEvents,
  projectConsoleLine,
  projectRawEvent,
  projectRichEvent,
  type EventEnvelope,
  type InvokeCommand,
  type SessionEventBatch,
} from "./fakeSession";

function event(
  sequence: number,
  kind = "message_delta",
  payload: unknown = { content: `message-${sequence}` },
  source: EventEnvelope["source"] = "cli",
  visibility: EventEnvelope["event"]["visibility"] = "user",
): EventEnvelope {
  return {
    event_id: `event-${sequence}`,
    session_id: "session-1",
    run_id: "run-1",
    sequence,
    timestamp: "2026-08-05T10:00:00Z",
    source,
    event: {
      kind,
      visibility,
      payload,
      vendor_event_id: null,
      raw_segment_reference: null,
    },
  };
}

describe("fake session event projections", () => {
  it("renders every exact raw byte without losing control or non-UTF-8 values", () => {
    expect(formatRawProtocolBytes([0x41, 0x0d, 0x0a, 0x09, 0x00, 0xff]))
      .toBe("A\\r\n\\t\\x00\\xff");
  });

  it("deduplicates, orders, and bounds normalized events by daemon sequence", () => {
    const merged = mergeSessionEvents(
      [event(4), event(2), event(3)],
      [event(3, "message", { content: "replacement" }), event(5), event(1)],
      4,
    );

    expect(merged.map((item) => item.sequence)).toEqual([2, 3, 4, 5]);
    expect(merged[1].event.kind).toBe("message");
    expect(mergeSessionEvents(merged, [event(6)], 0)).toEqual([]);
  });

  it("projects rich cards and explicit GUI to CLI console annotations", () => {
    const permission = event(8, "permission_request", {
      request_id: "permission-1",
      command: ["git", "status"],
    });
    const response = event(9, "gui_permission_response", {
      request_id: "permission-1",
      decision: "allow",
    }, "gui");

    expect(projectRichEvent(permission)).toMatchObject({
      title: "Permission required",
      detail: "git status",
      tone: "warning",
    });
    expect(projectConsoleLine(response)).toBe(
      "0009 GUI → CLI permission.allow(request_id=permission-1)",
    );
  });

  it("labels raw output as a projection and suppresses sensitive payloads", () => {
    const secret = "do-not-render-this-value";
    const raw = projectRawEvent(event(
      10,
      "user_input_result",
      { request_id: "input-1", value: secret },
      "cli",
      "sensitive",
    ));

    expect(raw).toContain('"projection": "redacted_normalized_event"');
    expect(raw).toContain('"redacted": true');
    expect(raw).not.toContain(secret);
    expect(projectConsoleLine(event(10, "user_input_result", { value: secret }, "cli", "sensitive")))
      .toContain("[REDACTED]");
  });
});

describe("fake session Tauri client", () => {
  it("keeps raw capture default-off and maps explicit launch/read opt-in", async () => {
    const invokeCommand: InvokeCommand = vi.fn(<T,>(command: string) => Promise.resolve(
      command === "session_raw_read"
        ? {
          capturedBytes: 3,
          complete: true,
          data: [1, 2, 3],
          nextOffset: 3,
          observedBytes: 3,
          runId: "run-1",
          sessionId: "session-1",
          truncated: false,
        }
        : { processId: 1, runId: "run-1", sessionId: "session-1" },
    ) as Promise<T>) as InvokeCommand;
    const client = createTauriFakeSessionClient(invokeCommand);

    await client.start("project-1", "structured/happy");
    await client.start("project-1", "structured/happy", null, null, true);
    await client.readRaw("session-1", "run-1", 0, 64);

    expect(invokeCommand).toHaveBeenNthCalledWith(1, "fake_session_start", expect.objectContaining({
      captureRawProtocol: false,
    }));
    expect(invokeCommand).toHaveBeenNthCalledWith(2, "fake_session_start", expect.objectContaining({
      captureRawProtocol: true,
    }));
    expect(invokeCommand).toHaveBeenNthCalledWith(3, "session_raw_read", {
      afterOffset: 0,
      maximumBytes: 64,
      runId: "run-1",
      sessionId: "session-1",
    });
  });

  it("maps structured/TUI attachment and bounded project session-list commands", async () => {
    const calls: Array<{ command: string; arguments_: Record<string, unknown> }> = [];
    const started = {
      sessionId: "session-tui-1",
      terminal: {
        canonicalCwd: "/tmp/project",
        processId: 17,
        runId: "run-tui-1",
        state: "running" as const,
        terminalId: "terminal-tui-1",
      },
    };
    const sessions = [{
      activeRunId: "run-structured-1",
      agentKind: "fake" as const,
      integrationMode: "structured" as const,
      latestSequence: 12,
      projectId: "project-1",
      sessionId: "session-structured-1",
      state: "completed" as const,
      title: "Fake · structured/happy",
      updatedAt: "2026-08-05T10:00:00Z",
    }];
    const structuredAttached = {
      processId: 18,
      runId: "run-structured-1",
      sessionId: "session-structured-1",
    };
    const invokeCommand: InvokeCommand = <T,>(command: string, arguments_: Record<string, unknown>) => {
      calls.push({ command, arguments_ });
      return Promise.resolve(
        command === "session_list"
          ? sessions
          : command === "fake_session_attach" ? structuredAttached : started,
      ) as Promise<T>;
    };
    const client = createTauriFakeSessionClient(invokeCommand);

    await expect(client.startTui("project-grant", "tui/alternate-screen", 100, 30))
      .resolves.toEqual(started);
    await expect(client.attachTui("session-tui-1")).resolves.toEqual(started);
    await expect(client.attach("project-grant", "session-structured-1"))
      .resolves.toEqual(structuredAttached);
    await expect(client.listSessions("project-grant", 25)).resolves.toEqual(sessions);
    expect(calls).toEqual([
      {
        command: "fake_tui_start",
        arguments_: {
          projectGrant: "project-grant",
          scenario: "tui/alternate-screen",
          columns: 100,
          rows: 30,
        },
      },
      {
        command: "fake_tui_attach",
        arguments_: { sessionId: "session-tui-1" },
      },
      {
        command: "fake_session_attach",
        arguments_: { projectGrant: "project-grant", sessionId: "session-structured-1" },
      },
      {
        command: "session_list",
        arguments_: { projectGrant: "project-grant", maximumSessions: 25 },
      },
    ]);
  });

  it("transports sensitive input and GUI payloads without writing them to console APIs", async () => {
    const calls: Array<{ command: string; arguments_: Record<string, unknown> }> = [];
    const invokeCommand: InvokeCommand = <T,>(command: string, arguments_: Record<string, unknown>) => {
      calls.push({ command, arguments_ });
      return Promise.resolve(command === "session_gui_action" ? "action-1" : undefined) as Promise<T>;
    };
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);
    const info = vi.spyOn(console, "info").mockImplementation(() => undefined);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const errorLog = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const client = createTauriFakeSessionClient(invokeCommand);
    const inputSecret = "private user response";
    const payloadSecret = "private action payload";

    await client.respondUserInput("session-1", "run-1", "input-1", inputSecret);
    await client.sendGuiAction("session-1", "run-1", "session.inspect", {
      privateValue: payloadSecret,
    });

    expect(calls).toEqual([
      {
        command: "session_user_input_respond",
        arguments_: {
          sessionId: "session-1",
          runId: "run-1",
          requestId: "input-1",
          valueJson: JSON.stringify(inputSecret),
        },
      },
      {
        command: "session_gui_action",
        arguments_: {
          sessionId: "session-1",
          runId: "run-1",
          action: "session.inspect",
          payloadJson: JSON.stringify({ privateValue: payloadSecret }),
        },
      },
    ]);
    expect(log).not.toHaveBeenCalled();
    expect(info).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
    expect(errorLog).not.toHaveBeenCalled();
  });

  it("aborts a pending long read without scheduling another frontend read", async () => {
    const invokeCommand: InvokeCommand = vi.fn(() => new Promise<SessionEventBatch>(() => undefined)) as InvokeCommand;
    const client = createTauriFakeSessionClient(invokeCommand);
    const controller = new AbortController();
    const read = client.readEvents("session-1", 12, controller.signal);

    controller.abort();

    await expect(read).rejects.toMatchObject({ name: "AbortError" });
    expect(invokeCommand).toHaveBeenCalledOnce();
  });
});
