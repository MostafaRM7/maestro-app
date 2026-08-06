import type { ITheme } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef } from "react";
import {
  createTerminalLinkHandler,
  createXtermRuntime,
  registerTerminalOscGuards,
  type TerminalFactory,
  type TerminalLinkRequestHandler,
  type TerminalRuntime,
  type TerminalTransport,
} from "../lib/terminal";

interface TerminalSurfaceProps {
  active: boolean;
  ariaLabel: string;
  factory?: TerminalFactory;
  fontSize?: number;
  onLinkRequest?: TerminalLinkRequestHandler;
  theme?: ITheme;
  transport: TerminalTransport;
}

export function TerminalSurface({
  active,
  ariaLabel,
  factory = createXtermRuntime,
  fontSize = 13,
  onLinkRequest,
  theme,
  transport,
}: TerminalSurfaceProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const runtimeRef = useRef<TerminalRuntime | null>(null);
  const initialAppearanceRef = useRef({ fontSize, theme });
  const activeRef = useRef(active);
  activeRef.current = active;

  useEffect(() => {
    if (!active) return;
    const host = hostRef.current;
    if (!host) return;

    const runtime = factory({
      allowProposedApi: false,
      convertEol: false,
      cursorBlink: false,
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
      fontSize: initialAppearanceRef.current.fontSize,
      linkHandler: createTerminalLinkHandler(onLinkRequest),
      scrollback: 50_000,
      theme: initialAppearanceRef.current.theme,
    });
    runtimeRef.current = runtime;
    runtime.terminal.open(host);

    const input = runtime.terminal.onData((data) => void transport.write(data));
    const resized = runtime.terminal.onResize(({ cols, rows }) => void transport.resize(cols, rows));
    const oscGuards = registerTerminalOscGuards(runtime.terminal);
    const pendingWrites = new Set<() => void>();
    const unsubscribe = transport.subscribe((data) => new Promise<void>((resolve) => {
      const complete = () => {
        pendingWrites.delete(complete);
        resolve();
      };
      pendingWrites.add(complete);
      runtime.terminal.write(data, complete);
    }));
    const observer = new ResizeObserver(() => {
      if (activeRef.current) runtime.fit.fit();
    });
    observer.observe(host);
    const frame = requestAnimationFrame(() => {
      if (activeRef.current) runtime.fit.fit();
    });

    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      unsubscribe();
      for (const complete of pendingWrites) complete();
      oscGuards.dispose();
      resized.dispose();
      input.dispose();
      runtime.fit.dispose();
      runtime.terminal.dispose();
      runtimeRef.current = null;
    };
  }, [active, factory, onLinkRequest, transport]);

  useEffect(() => {
    const runtime = runtimeRef.current;
    if (!runtime) return;
    runtime.terminal.options.fontSize = fontSize;
    runtime.terminal.options.theme = theme;
    if (active) runtime.fit.fit();
  }, [active, fontSize, theme]);

  return <div aria-label={ariaLabel} className="terminal-surface" data-terminal-input="true" ref={hostRef} role="region" />;
}
