import { FitAddon } from "@xterm/addon-fit";
import {
  Terminal,
  type IDisposable,
  type ILinkHandler,
  type ITerminalOptions,
} from "@xterm/xterm";

const MAX_CONFIRMABLE_LINK_BYTES = 2_048;
const PROTECTED_OSC_IDENTIFIERS = [0, 2, 52] as const;

export interface TerminalTransport {
  resize: (columns: number, rows: number) => void | Promise<void>;
  subscribe: (listener: (data: string | Uint8Array) => void | Promise<void>) => () => void;
  write: (data: string) => void | Promise<void>;
}

export interface TerminalAdapter {
  readonly cols: number;
  readonly rows: number;
  dispose: () => void;
  onData: (listener: (data: string) => void) => IDisposable;
  onResize: (listener: (size: { cols: number; rows: number }) => void) => IDisposable;
  open: (element: HTMLElement) => void;
  options: ITerminalOptions;
  parser: {
    registerOscHandler: (identifier: number, callback: (data: string) => boolean) => IDisposable;
  };
  write: (data: string | Uint8Array, callback?: () => void) => void;
}

export interface FitAdapter {
  dispose: () => void;
  fit: () => void;
}

export interface TerminalRuntime {
  fit: FitAdapter;
  terminal: TerminalAdapter;
}

export type TerminalFactory = (options: ITerminalOptions) => TerminalRuntime;

export interface TerminalLinkRequest {
  readonly requiresConfirmation: true;
  readonly trusted: false;
  readonly url: string;
}

export type TerminalLinkRequestHandler = (request: TerminalLinkRequest) => void;

/**
 * Produces a handler that can only request confirmation for bounded HTTP(S)
 * links. It never opens a URL itself and rejects embedded credentials.
 */
export function createTerminalLinkHandler(
  requestConfirmation?: TerminalLinkRequestHandler,
): ILinkHandler {
  return {
    activate: (_event, text) => {
      const request = terminalLinkRequest(text);
      if (request) requestConfirmation?.(request);
    },
    allowNonHttpProtocols: false,
  };
}

/**
 * Claims application-title and clipboard OSC sequences before xterm's default
 * handlers. Returning true consumes the payload without exposing it to the UI.
 */
export function registerTerminalOscGuards(
  terminal: Pick<TerminalAdapter, "parser">,
): IDisposable {
  const guards = PROTECTED_OSC_IDENTIFIERS.map((identifier) =>
    terminal.parser.registerOscHandler(identifier, () => true)
  );
  return {
    dispose() {
      for (const guard of guards.reverse()) guard.dispose();
    },
  };
}

function terminalLinkRequest(text: string): TerminalLinkRequest | null {
  if (new TextEncoder().encode(text).byteLength > MAX_CONFIRMABLE_LINK_BYTES) return null;
  try {
    const parsed = new URL(text);
    if (
      (parsed.protocol !== "http:" && parsed.protocol !== "https:")
      || parsed.username
      || parsed.password
    ) {
      return null;
    }
    const sensitiveQueryNames = new Set([
      "access_token",
      "api_key",
      "apikey",
      "auth",
      "authorization",
      "key",
      "password",
      "secret",
      "signature",
      "token",
    ]);
    if (Array.from(parsed.searchParams.keys()).some((name) => sensitiveQueryNames.has(name.toLowerCase()))) {
      return null;
    }
    return { requiresConfirmation: true, trusted: false, url: parsed.href };
  } catch {
    return null;
  }
}

export const createXtermRuntime: TerminalFactory = (options) => {
  const terminal = new Terminal(options);
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  return { fit, terminal };
};
