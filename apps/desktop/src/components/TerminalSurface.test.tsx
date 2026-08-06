import type { IDisposable, ITerminalOptions } from "@xterm/xterm";
import { act, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  type TerminalAdapter,
  type TerminalFactory,
  type TerminalTransport,
} from "../lib/terminal";
import { TerminalSurface } from "./TerminalSurface";

function disposable() {
  const dispose = vi.fn();
  return [{ dispose } satisfies IDisposable, dispose] as const;
}

describe("TerminalSurface", () => {
  it("fits, bridges bytes and input, reports resize, and cleans up every resource", async () => {
    let output: ((data: string | Uint8Array) => void | Promise<void>) | undefined;
    let input: ((data: string) => void) | undefined;
    let resize: ((size: { cols: number; rows: number }) => void) | undefined;
    const [inputDisposable, inputDispose] = disposable();
    const [resizeDisposable, resizeDispose] = disposable();
    const oscDisposes = [vi.fn(), vi.fn(), vi.fn()];
    const oscHandlers = new Map<number, (data: string) => boolean>();
    const registerOscHandler = vi.fn((identifier: number, callback: (data: string) => boolean) => {
      oscHandlers.set(identifier, callback);
      return { dispose: oscDisposes[oscHandlers.size - 1] };
    });
    const unsubscribe = vi.fn();
    const terminalDispose = vi.fn();
    const terminalOpen = vi.fn<(element: HTMLElement) => void>();
    let completeWrite: (() => void) | undefined;
    const terminalWrite = vi.fn<(data: string | Uint8Array, callback?: () => void) => void>((_data, callback) => {
      completeWrite = callback;
    });
    const terminal: TerminalAdapter = {
      cols: 80,
      rows: 24,
      dispose: terminalDispose,
      onData: (listener) => { input = listener; return inputDisposable; },
      onResize: (listener) => { resize = listener; return resizeDisposable; },
      open: terminalOpen,
      options: {} as ITerminalOptions,
      parser: { registerOscHandler },
      write: terminalWrite,
    };
    const fitDispose = vi.fn();
    const fitNow = vi.fn();
    const fit = { dispose: fitDispose, fit: fitNow };
    const factory = vi.fn<TerminalFactory>(() => ({ fit, terminal }));
    const transportResize = vi.fn<(columns: number, rows: number) => void>();
    const transportWrite = vi.fn<(data: string) => void>();
    const transport: TerminalTransport = {
      resize: transportResize,
      subscribe: (listener) => { output = listener; return unsubscribe; },
      write: transportWrite,
    };
    const onLinkRequest = vi.fn();

    const view = render(
      <TerminalSurface
        active
        ariaLabel="Test shell"
        factory={factory}
        onLinkRequest={onLinkRequest}
        transport={transport}
      />,
    );
    await act(async () => Promise.resolve());

    expect(view.getByRole("region", { name: "Test shell" })).toHaveAttribute("data-terminal-input", "true");
    expect(terminalOpen).toHaveBeenCalledOnce();
    expect(fitNow).toHaveBeenCalled();
    act(() => { void output?.(new Uint8Array([65, 66])); });
    expect(terminalWrite).toHaveBeenCalledWith(new Uint8Array([65, 66]), expect.any(Function));
    act(() => completeWrite?.());
    act(() => input?.("ls\r"));
    expect(transportWrite).toHaveBeenCalledWith("ls\r");
    act(() => resize?.({ cols: 120, rows: 40 }));
    expect(transportResize).toHaveBeenCalledWith(120, 40);

    view.unmount();
    expect(unsubscribe).toHaveBeenCalledOnce();
    expect(inputDispose).toHaveBeenCalledOnce();
    expect(resizeDispose).toHaveBeenCalledOnce();
    expect(registerOscHandler.mock.calls.map(([identifier]) => identifier)).toEqual([0, 2, 52]);
    expect(oscHandlers.get(0)?.("hostile-title")).toBe(true);
    expect(oscHandlers.get(2)?.("hostile-icon-and-title")).toBe(true);
    expect(oscHandlers.get(52)?.("c;c2VjcmV0")).toBe(true);
    for (const dispose of oscDisposes) expect(dispose).toHaveBeenCalledOnce();
    expect(fitDispose).toHaveBeenCalledOnce();
    expect(terminalDispose).toHaveBeenCalledOnce();
    const options = factory.mock.calls[0]?.[0];
    expect(options?.convertEol).toBe(false);
    expect(options?.linkHandler?.allowNonHttpProtocols).toBe(false);
    options?.linkHandler?.activate(
      undefined as never,
      "https://example.invalid/review path",
      undefined as never,
    );
    expect(onLinkRequest).toHaveBeenCalledWith({
      requiresConfirmation: true,
      trusted: false,
      url: "https://example.invalid/review%20path",
    });
    options?.linkHandler?.activate(undefined as never, "javascript:alert(1)", undefined as never);
    options?.linkHandler?.activate(undefined as never, "https://user:secret@example.invalid", undefined as never);
    options?.linkHandler?.activate(undefined as never, "https://example.invalid/?token=fixture", undefined as never);
    expect(onLinkRequest).toHaveBeenCalledOnce();
  });

  it("does not create or feed xterm while inactive and replays on each activation", async () => {
    const terminalWrites: Array<ReturnType<typeof vi.fn>> = [];
    const terminalDisposes: Array<ReturnType<typeof vi.fn>> = [];
    const factory = vi.fn<TerminalFactory>(() => {
      const write = vi.fn<(data: string | Uint8Array, callback?: () => void) => void>((_data, callback) => callback?.());
      const dispose = vi.fn();
      terminalWrites.push(write);
      terminalDisposes.push(dispose);
      return {
        fit: { dispose: vi.fn(), fit: vi.fn() },
        terminal: {
          cols: 80,
          rows: 24,
          dispose,
          onData: () => ({ dispose: vi.fn() }),
          onResize: () => ({ dispose: vi.fn() }),
          open: vi.fn(),
          options: {} as ITerminalOptions,
          parser: { registerOscHandler: () => ({ dispose: vi.fn() }) },
          write,
        },
      };
    });
    const unsubscribes: Array<ReturnType<typeof vi.fn>> = [];
    const transport: TerminalTransport = {
      resize: vi.fn(),
      subscribe: (listener) => {
        const unsubscribe = vi.fn();
        unsubscribes.push(unsubscribe);
        void listener(new Uint8Array([65, 66]));
        return unsubscribe;
      },
      write: vi.fn(),
    };
    const view = render(<TerminalSurface active={false} ariaLabel="Paused shell" factory={factory} transport={transport} />);

    expect(factory).not.toHaveBeenCalled();
    view.rerender(<TerminalSurface active ariaLabel="Paused shell" factory={factory} transport={transport} />);
    await act(async () => Promise.resolve());
    expect(factory).toHaveBeenCalledOnce();
    expect(terminalWrites[0]).toHaveBeenCalledWith(new Uint8Array([65, 66]), expect.any(Function));

    view.rerender(<TerminalSurface active={false} ariaLabel="Paused shell" factory={factory} transport={transport} />);
    expect(unsubscribes[0]).toHaveBeenCalledOnce();
    expect(terminalDisposes[0]).toHaveBeenCalledOnce();

    view.rerender(<TerminalSurface active ariaLabel="Paused shell" factory={factory} transport={transport} />);
    await act(async () => Promise.resolve());
    expect(factory).toHaveBeenCalledTimes(2);
    expect(terminalWrites[1]).toHaveBeenCalledWith(new Uint8Array([65, 66]), expect.any(Function));
  });
});
