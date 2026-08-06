import { Terminal } from "@xterm/xterm";
import { describe, expect, it, vi } from "vitest";
import { registerTerminalOscGuards } from "./terminal";

const VT_BASELINE = "\u001b[2J\u001b[H\u001b[32mMaestro fake TUI ✓\u001b[0m\r\n> ";
const ENTER_ALTERNATE_SCREEN = "main-before\r\n\u001b[?1049h\u001b[2J\u001b[Halternate";
const EXIT_ALTERNATE_SCREEN = "\u001b[?1049lmain-after\r\n";
const HOSTILE_OSC = "\u001b]0;hostile-title\u0007\u001b]52;c;c2VjcmV0\u0007\u001b]8;;https://example.invalid/?token=fixture\u0007link\u001b]8;;\u0007\r\n";

interface CoreMouseEvent {
  action: number;
  alt: boolean;
  button: number;
  col: number;
  ctrl: boolean;
  row: number;
  shift: boolean;
  x: number;
  y: number;
}

interface XtermMouseCore {
  _core: {
    coreMouseService: {
      triggerMouseEvent: (event: CoreMouseEvent) => boolean;
    };
  };
}

function write(terminal: Terminal, data: string | Uint8Array) {
  return new Promise<void>((resolve) => terminal.write(data, resolve));
}

function mouseEvent(
  col: number,
  row: number,
  button: number,
  action: number,
): CoreMouseEvent {
  return {
    action,
    alt: false,
    button,
    col,
    ctrl: false,
    row,
    shift: false,
    x: 0,
    y: 0,
  };
}

describe("the production xterm compatibility contract", () => {
  it("renders the canonical VT screen with exact cursor, ANSI color, and Unicode", async () => {
    const terminal = new Terminal({ cols: 40, convertEol: false, rows: 6 });
    const encoder = new TextEncoder();
    const baselineBytes = encoder.encode(VT_BASELINE);
    const checkmarkStart = encoder.encode(VT_BASELINE.slice(0, VT_BASELINE.indexOf("✓"))).length;

    await write(terminal, baselineBytes.slice(0, checkmarkStart + 1));
    await write(terminal, baselineBytes.slice(checkmarkStart + 1));

    const first = terminal.buffer.active.getLine(0);
    const prompt = terminal.buffer.active.getLine(1);
    expect(terminal.buffer.active.type).toBe("normal");
    expect(first?.translateToString(true)).toBe("Maestro fake TUI ✓");
    expect(prompt?.translateToString(true)).toBe("> ");
    expect(terminal.buffer.active.cursorX).toBe(2);
    expect(terminal.buffer.active.cursorY).toBe(1);
    expect(first?.getCell(0)?.isFgPalette()).toBe(true);
    expect(first?.getCell(0)?.getFgColor()).toBe(2);
    expect(prompt?.getCell(0)?.isFgDefault()).toBe(true);

    await write(terminal, "hello\r\n");
    await write(terminal, "echo: hello\r\n");

    expect(terminal.buffer.active.getLine(0)?.translateToString(true)).toBe("Maestro fake TUI ✓");
    expect(terminal.buffer.active.getLine(1)?.translateToString(true)).toBe("> hello");
    expect(terminal.buffer.active.getLine(2)?.translateToString(true)).toBe("echo: hello");
    expect(terminal.buffer.active.getLine(3)?.translateToString(true)).toBe("");
    expect(terminal.buffer.active.cursorX).toBe(0);
    expect(terminal.buffer.active.cursorY).toBe(3);
    terminal.dispose();
  });

  it("restores the normal screen and never leaks alternate content into scrollback", async () => {
    const terminal = new Terminal({ cols: 40, rows: 6, scrollback: 50 });

    await write(terminal, ENTER_ALTERNATE_SCREEN);

    expect(terminal.buffer.active.type).toBe("alternate");
    expect(terminal.buffer.alternate.getLine(0)?.translateToString(true)).toBe("alternate");
    expect(terminal.buffer.alternate.cursorX).toBe(9);
    expect(terminal.buffer.alternate.cursorY).toBe(0);

    await write(terminal, EXIT_ALTERNATE_SCREEN);

    expect(terminal.buffer.active.type).toBe("normal");
    expect(terminal.buffer.normal.getLine(0)?.translateToString(true)).toBe("main-before");
    expect(terminal.buffer.normal.getLine(1)?.translateToString(true)).toBe("main-after");
    expect(terminal.buffer.normal.cursorX).toBe(0);
    expect(terminal.buffer.normal.cursorY).toBe(2);
    const normalContents = Array.from(
      { length: terminal.buffer.normal.length },
      (_, index) => terminal.buffer.normal.getLine(index)?.translateToString(true) ?? "",
    );
    expect(normalContents).not.toContain("alternate");
    terminal.dispose();
  });

  it("emits exact SGR press, release, motion, and wheel reports after resize", async () => {
    const terminal = new Terminal({ cols: 20, rows: 10 });
    const reports: string[] = [];
    const sizes: Array<{ cols: number; rows: number }> = [];
    terminal.onData((data) => reports.push(data));
    terminal.onResize((size) => sizes.push(size));
    await write(terminal, "\u001b[?1003h\u001b[?1006h");
    terminal.resize(120, 40);
    const mouse = (terminal as unknown as XtermMouseCore)._core.coreMouseService;

    expect(mouse.triggerMouseEvent(mouseEvent(4, 2, 0, 1))).toBe(true);
    expect(mouse.triggerMouseEvent(mouseEvent(4, 2, 0, 0))).toBe(true);
    expect(mouse.triggerMouseEvent(mouseEvent(5, 3, 0, 32))).toBe(true);
    expect(mouse.triggerMouseEvent(mouseEvent(6, 4, 4, 0))).toBe(true);
    expect(mouse.triggerMouseEvent(mouseEvent(7, 5, 4, 1))).toBe(true);

    expect(sizes).toEqual([{ cols: 120, rows: 40 }]);
    expect(reports).toEqual([
      "\u001b[<0;5;3M",
      "\u001b[<0;5;3m",
      "\u001b[<32;6;4M",
      "\u001b[<64;7;5M",
      "\u001b[<65;8;6M",
    ]);
    terminal.dispose();
  });

  it("consumes title and clipboard OSC while rendering hyperlink text only", async () => {
    const terminal = new Terminal({ cols: 80, rows: 4 });
    const titleChange = vi.fn();
    const titleSubscription = terminal.onTitleChange(titleChange);
    const guards = registerTerminalOscGuards(terminal);

    await write(terminal, HOSTILE_OSC);

    expect(titleChange).not.toHaveBeenCalled();
    expect(terminal.buffer.active.getLine(0)?.translateToString(true)).toBe("link");
    expect(document.title).not.toBe("hostile-title");
    guards.dispose();
    titleSubscription.dispose();
    terminal.dispose();
  });
});
