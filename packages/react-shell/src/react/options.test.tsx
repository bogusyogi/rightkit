// Markup-level checks for the 0.6.0 options that render on the server. Behaviour (focus, keys, timing)
// is covered in e2e/options.e2e.test.ts.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ActionDialog, ConfirmDialog, Dialog, ModalShell } from "./overlays.js";
import { CommandPalette, rankCommands } from "./palette.js";

const noop = () => undefined;
const commands = [
  { id: "a", label: "Open file", run: noop },
  { id: "b", label: "Save file", keywords: ["write"], run: noop },
];

describe("ModalShell and Dialog options", () => {
  it("overlay class, slot and style land on the backdrop", () => {
    const html = renderToStaticMarkup(<ModalShell label="x" onClose={noop} overlayClassName="o1" slots={{ overlay: "o2" }} overlayStyle={{ background: "red" }}>b</ModalShell>);
    expect(html).toContain('class="rk-overlay rk-overlay--center o1 o2" style="background:red"');
  });
  it("ariaLabel replaces aria-labelledby, also through Dialog", () => {
    const shell = renderToStaticMarkup(<ModalShell label="x" labelledBy="h" ariaLabel="Named" onClose={noop}>b</ModalShell>);
    expect(shell).toContain('aria-label="Named"');
    expect(shell).not.toContain("aria-labelledby");
    const dialog = renderToStaticMarkup(<Dialog title="T" ariaLabel="Named" onClose={noop} />);
    expect(dialog).toContain('aria-label="Named"');
    expect(dialog).not.toContain("aria-labelledby");
    expect(dialog).toContain("<h2"); // the visible title stays
  });
  it("Dialog slots.overlay reaches the backdrop", () => {
    expect(renderToStaticMarkup(<Dialog title="T" slots={{ overlay: "ov" }} onClose={noop} />)).toContain("rk-overlay--center ov");
  });
});

describe("ActionDialog and ConfirmDialog", () => {
  it("renders N actions in order with variant and class, and an explicit role", () => {
    const html = renderToStaticMarkup(
      <ActionDialog
        title="Save?"
        role="alertdialog"
        onClose={noop}
        actions={[
          { id: "1", label: "Don't save", variant: "danger", onClick: noop },
          { id: "2", label: "Cancel", onClick: noop, className: "mid" },
          { id: "3", label: "Save", variant: "primary", onClick: noop, disabled: true },
        ]}
      />,
    );
    expect(html).toContain('role="alertdialog"');
    const buttons = [...html.matchAll(/<button type="button" class="([^"]*)"[^>]*>([^<]*)<\/button>/g)].map((m) => [m[1], m[2]]);
    expect(buttons).toEqual([
      ["rk-btn rk-btn--danger", "Don&#x27;t save"],
      ["rk-btn rk-btn--secondary mid", "Cancel"],
      ["rk-btn rk-btn--primary", "Save"],
    ]);
    expect(html).toContain("disabled");
  });
  it("defaults to role=dialog", () => {
    expect(renderToStaticMarkup(<ActionDialog title="T" onClose={noop} actions={[{ label: "OK", onClick: noop }]} />)).toContain('role="dialog"');
  });
  it("ConfirmDialog role override wins over danger", () => {
    expect(renderToStaticMarkup(<ConfirmDialog title="T" danger role="dialog" onConfirm={noop} onCancel={noop} />)).toContain('role="dialog"');
    expect(renderToStaticMarkup(<ConfirmDialog title="T" role="alertdialog" onConfirm={noop} onCancel={noop} />)).toContain('role="alertdialog"');
  });
});

describe("CommandPalette options", () => {
  it("inputAriaLabel is separate from the placeholder; both default to the same string", () => {
    const split = renderToStaticMarkup(<CommandPalette commands={commands} onClose={noop} labels={{ inputAriaLabel: "Search files", placeholder: "Type…" }} />);
    expect(split).toContain('aria-label="Search files"');
    expect(split).toContain('placeholder="Type…"');
    const same = renderToStaticMarkup(<CommandPalette commands={commands} onClose={noop} labels={{ placeholder: "Type…" }} />);
    expect(same).toContain('aria-label="Type…" aria-expanded');
  });
  it("rank replaces the built-in ordering; rankCommands is the built-in", () => {
    const only = renderToStaticMarkup(<CommandPalette commands={commands} onClose={noop} rank={(list) => list.filter((c) => c.id === "b")} />);
    expect(only).toContain("Save file");
    expect(only).not.toContain("Open file");
    expect(rankCommands(commands, "write").map((c) => c.id)).toEqual(["b"]);
    expect(rankCommands(commands, "").map((c) => c.id)).toEqual(["a", "b"]);
    expect(rankCommands(commands, "", 1)).toHaveLength(1);
  });
  it("searchIcon and renderIcon replace the built-in icons", () => {
    const html = renderToStaticMarkup(
      <CommandPalette commands={[{ id: "a", label: "A", icon: <i>old</i>, run: noop }]} onClose={noop} searchIcon={<em>S</em>} renderIcon={(c) => <u>{c.id}</u>} slots={{ icon: "ic" }} />,
    );
    expect(html).toContain("<em>S</em>");
    expect(html).not.toContain("<svg");
    expect(html).toContain('class="rk-palette__icon ic" aria-hidden="true"><u>a</u>');
    expect(renderToStaticMarkup(<CommandPalette commands={commands} onClose={noop} searchIcon={null} />)).not.toContain("<svg");
  });
});
