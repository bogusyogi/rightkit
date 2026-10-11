// Pins the 0.5.1 markup of the server-renderable overlays so new options cannot change default output.
// (Context menu and toast rows need a DOM; e2e/default-output.e2e.test.ts pins those in a real browser.)
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ConfirmDialog, Dialog, ModalShell, PromptDialog } from "./overlays.js";
import { CommandPalette } from "./palette.js";
import { ToastViewport } from "./toasts.js";
import { createToastBus } from "../toast.js";

const noop = () => undefined;

describe("overlay default markup", () => {
  it("ModalShell: centered, top, labelled-by, described-by, role override", () => {
    expect(renderToStaticMarkup(<ModalShell label="Edit" onClose={noop}><p>body</p></ModalShell>)).toMatchSnapshot("center");
    expect(renderToStaticMarkup(<ModalShell label="Edit" onClose={noop} align="top" width={300} className="x" slots={{ root: "y" }}><p>body</p></ModalShell>)).toMatchSnapshot("top");
    expect(renderToStaticMarkup(<ModalShell label="Edit" onClose={noop} labelledBy="t" describedBy="d" role="alertdialog"><p>body</p></ModalShell>)).toMatchSnapshot("aria");
  });
  it("Dialog: title only, with description, actions and body slot", () => {
    expect(renderToStaticMarkup(<Dialog title="Rename" onClose={noop} />)).toMatchSnapshot("title");
    expect(renderToStaticMarkup(<Dialog title="Rename" description="Pick a name" actions={<button type="button">OK</button>} slots={{ body: "b", header: "h", footer: "f", root: "r" }} onClose={noop}><p>x</p></Dialog>)).toMatchSnapshot("full");
  });
  it("ConfirmDialog: plain and danger", () => {
    expect(renderToStaticMarkup(<ConfirmDialog title="Delete?" description="Gone" onConfirm={noop} onCancel={noop} />)).toMatchSnapshot("plain");
    expect(renderToStaticMarkup(<ConfirmDialog title="Delete?" danger confirmLabel="Delete" cancelLabel="Keep" onConfirm={noop} onCancel={noop} className="c" slots={{ footer: "f" }} />)).toMatchSnapshot("danger");
  });
  it("PromptDialog", () => {
    expect(renderToStaticMarkup(<PromptDialog title="Rename" label="Name" initialValue="a" placeholder="p" onSubmit={noop} onCancel={noop} />)).toMatchSnapshot();
  });
  it("CommandPalette: grouped results, custom labels, slots, and empty", () => {
    const commands = [
      { id: "a", label: "Open file", group: "File", keywords: ["load"], hint: "recent", icon: <i>i</i>, run: noop },
      { id: "b", label: "Save", group: "File", chord: "mod+s", run: noop },
      { id: "c", label: "Toggle theme", run: noop },
    ];
    expect(renderToStaticMarkup(<CommandPalette commands={commands} onClose={noop} />)).toMatchSnapshot("grouped");
    expect(renderToStaticMarkup(<CommandPalette commands={commands} onClose={noop} labels={{ placeholder: "Find", title: "Palette", empty: "None", results: "Hits" }} paletteWidth={500} className="p" slots={{ root: "r", field: "f", input: "i", list: "l", group: "g", option: "o", empty: "e" }} />)).toMatchSnapshot("labels");
    expect(renderToStaticMarkup(<CommandPalette commands={[]} onClose={noop} />)).toMatchSnapshot("empty");
  });
  it("ToastViewport: empty live region", () => {
    expect(renderToStaticMarkup(<ToastViewport bus={createToastBus()} className="t" slots={{ root: "r" }} />)).toMatchSnapshot();
  });
});
