import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { CommandPalette } from "./palette.js";
import { Dialog, ModalShell } from "./overlays.js";
import { ToastViewport } from "./toasts.js";
import { Titlebar } from "./layout.js";
import { cx } from "./ui.js";

const classesOf = (html: string) => [...html.matchAll(/class="([^"]*)"/g)].map((m) => m[1]);

describe("className and slots", () => {
  it("merges class names with the rk classes without dropping them", () => {
    expect(cx("a", false, undefined, "b")).toBe("a b");
  });

  it("applies className and slots.root to a modal surface", () => {
    const html = renderToStaticMarkup(
      <ModalShell label="Edit" onClose={() => undefined} className="app-modal" slots={{ root: "app-modal-root" }}>
        <p>body</p>
      </ModalShell>,
    );
    expect(classesOf(html).some((c) => c.includes("rk-modal") && c.includes("app-modal") && c.includes("app-modal-root"))).toBe(true);
  });

  it("applies header, body, footer and root slots to a Dialog", () => {
    const html = renderToStaticMarkup(
      <Dialog
        title="Rename"
        slots={{ root: "d-root", header: "d-head", body: "d-body", footer: "d-foot" }}
        actions={<button type="button">OK</button>}
        onClose={() => undefined}
      >
        <p>content</p>
      </Dialog>,
    );
    const all = classesOf(html).join(" ");
    expect(all).toContain("d-root");
    expect(all).toContain("rk-modal__title d-head");
    expect(all).toContain("rk-modal__body d-body");
    expect(all).toContain("rk-modal__actions d-foot");
  });

  it("keeps the Dialog DOM unchanged when no body slot is given", () => {
    const html = renderToStaticMarkup(<Dialog title="Plain" onClose={() => undefined}><p>x</p></Dialog>);
    expect(html).not.toContain("rk-modal__body");
  });

  it("renders the palette at the requested width with slot classes", () => {
    const html = renderToStaticMarkup(
      <CommandPalette commands={[]} onClose={() => undefined} paletteWidth={520} className="p-app" slots={{ root: "p-root", field: "p-field", empty: "p-empty" }} />,
    );
    expect(html).toContain("width:520px");
    expect(html).toContain("p-app");
    expect(html).toContain("p-root");
    expect(html).toContain("p-field");
    expect(html).toContain("p-empty");
  });

  it("uses the default 640px palette width", () => {
    const html = renderToStaticMarkup(<CommandPalette commands={[]} onClose={() => undefined} />);
    expect(html).toContain("width:640px");
  });

  it("applies slot classes to the toast live region", () => {
    const html = renderToStaticMarkup(<ToastViewport className="t-app" slots={{ root: "t-root" }} />);
    expect(html).toContain("t-app");
    expect(html).toContain("t-root");
    expect(html).toContain("rk-toasts");
  });

  it("applies titlebar slots and keeps the default classes", () => {
    const html = renderToStaticMarkup(
      <Titlebar platform="windows" title="Doc" slots={{ root: "tb", side: "tb-side", title: "tb-title", drag: "tb-drag", end: "tb-end", caption: "tb-cap" }} />,
    );
    const all = classesOf(html).join(" ");
    for (const token of ["rk-top tb", "rk-top__side tb-side", "rk-top__title tb-title", "rk-top__drag tb-drag"]) {
      expect(all).toContain(token);
    }
  });
});
