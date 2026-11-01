// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ModalShell } from "@rightkit/react-shell/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkdownLinkPopover } from "./references/MarkdownLinkPopover";
import { useSlashMenuEscape } from "./useSlashMenuEscape";

// No vitest globals: unmount between tests so a later test never reads an earlier test's DOM.
afterEach(cleanup);

const model = {
  mode: "insert",
  from: 0,
  to: 0,
  text: "",
  address: "",
  title: "",
} as never;

function Harness({
  close,
  cancel,
  open = true,
}: {
  close: (r: boolean) => void;
  cancel: () => void;
  open?: boolean;
}) {
  useSlashMenuEscape(open, close);
  return <MarkdownLinkPopover model={model} onApply={() => null} onCancel={cancel} />;
}

describe("slash menu over link popover", () => {
  it("one Escape with focus in the popover closes only the slash menu", () => {
    const close = vi.fn();
    const cancel = vi.fn();
    render(<Harness close={close} cancel={cancel} />);
    const input = screen.getAllByRole("textbox")[0];
    input.focus();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(close).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledWith(true);
    expect(cancel).not.toHaveBeenCalled();
  });

  it("with the slash menu closed, one Escape in the popover cancels it", () => {
    const close = vi.fn();
    const cancel = vi.fn();
    render(<Harness close={close} cancel={cancel} open={false} />);
    const input = screen.getAllByRole("textbox")[0];
    input.focus();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(close).not.toHaveBeenCalled();
  });
});

// The image dialog is the shell's `ModalShell`: it reads Escape on the window in the capture phase and
// stops it (react-shell overlays.js), which is before the slash menu's document-capture listener, so an
// open slash menu stays open under a dialog and only the dialog closes.
function ModalHarness({ close, dialogClose }: { close: (r: boolean) => void; dialogClose: () => void }) {
  useSlashMenuEscape(true, close);
  return (
    <ModalShell label="Insert image" onClose={dialogClose} autoFocus={false}>
      <input aria-label="alt text" />
    </ModalShell>
  );
}

describe("slash menu under the image dialog", () => {
  it("one Escape closes the dialog only, not the slash menu beneath it", async () => {
    const close = vi.fn();
    const dialogClose = vi.fn();
    render(<ModalHarness close={close} dialogClose={dialogClose} />);
    const input = screen.getByLabelText("alt text");
    input.focus();
    fireEvent.keyDown(input, { key: "Escape" });
    await waitFor(() => expect(dialogClose).toHaveBeenCalledTimes(1));
    expect(close).not.toHaveBeenCalled();
  });
});
