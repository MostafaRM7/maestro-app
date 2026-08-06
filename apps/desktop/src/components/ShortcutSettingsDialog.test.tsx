import { render, screen } from "@testing-library/react";
import { useState } from "react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { defaultShortcutBindings } from "../lib/shortcuts";
import { ShortcutSettingsDialog } from "./ShortcutSettingsDialog";

describe("ShortcutSettingsDialog", () => {
  it("blocks conflicts and saves a normalized remapping", async () => {
    const onSave = vi.fn().mockResolvedValue(true);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<ShortcutSettingsDialog bindings={defaultShortcutBindings()} error={null} loading={false} onClose={onClose} onSave={onSave} />);

    const openProject = screen.getByLabelText("Open project");
    expect(openProject).toHaveFocus();
    await user.clear(openProject);
    await user.type(openProject, "Mod+B");
    expect(screen.getByText(/assigned more than once/u)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save shortcuts" })).toBeDisabled();

    await user.clear(openProject);
    await user.type(openProject, "mod+l");
    await user.click(screen.getByRole("button", { name: "Save shortcuts" }));
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ openProject: "Mod+L" }));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("adopts asynchronously loaded bindings before editing and blocks restore while loading", () => {
    const initial = defaultShortcutBindings();
    const loaded = { ...initial, openProject: "Mod+L" };
    const view = render(<ShortcutSettingsDialog bindings={initial} error={null} loading onClose={vi.fn()} onSave={vi.fn()} />);

    expect(screen.getByRole("button", { name: "Restore defaults" })).toBeDisabled();
    view.rerender(<ShortcutSettingsDialog bindings={loaded} error={null} loading={false} onClose={vi.fn()} onSave={vi.fn()} />);

    expect(screen.getByLabelText("Open project")).toHaveValue("Mod+L");
  });

  it("returns focus to the control that opened the modal", async () => {
    const user = userEvent.setup();
    render(<ShortcutDialogHarness />);
    const opener = screen.getByRole("button", { name: "Keyboard shortcut settings" });
    opener.focus();
    await user.click(opener);
    expect(screen.getByLabelText("Open project")).toHaveFocus();

    await user.keyboard("{Escape}");

    expect(screen.queryByRole("dialog", { name: "Keyboard shortcuts" })).not.toBeInTheDocument();
    expect(opener).toHaveFocus();
  });
});

function ShortcutDialogHarness() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button onClick={() => setOpen(true)} type="button">Keyboard shortcut settings</button>
      {open ? (
        <ShortcutSettingsDialog
          bindings={defaultShortcutBindings()}
          error={null}
          loading={false}
          onClose={() => setOpen(false)}
          onSave={vi.fn()}
        />
      ) : null}
    </>
  );
}
