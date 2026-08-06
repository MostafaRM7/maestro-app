import { describe, expect, it, vi } from "vitest";
import { createTerminalLinkRequestHandler } from "./terminalLinkClient";

describe("terminal link client", () => {
  it("passes only the normalized URL to the native confirmation command", () => {
    const invoke = vi.fn().mockResolvedValue(true);
    const request = createTerminalLinkRequestHandler(invoke);

    request({
      requiresConfirmation: true,
      trusted: false,
      url: "https://example.invalid/review",
    });

    expect(invoke).toHaveBeenCalledWith("terminal_link_open", {
      url: "https://example.invalid/review",
    });
  });
});
