import { invoke } from "@tauri-apps/api/core";
import type { TerminalLinkRequestHandler } from "./terminal";

type InvokeCommand = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export function createTerminalLinkRequestHandler(
  invokeCommand: InvokeCommand = invoke,
): TerminalLinkRequestHandler {
  return ({ url }) => {
    void invokeCommand<boolean>("terminal_link_open", { url });
  };
}

export const requestTerminalLinkOpen = createTerminalLinkRequestHandler();
