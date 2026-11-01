import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { nextDropOverlayVisible } from "../../src/queue/lib/dropOverlay";

describe("drop overlay follows native drag-drop phases (#173)", () => {
  it("shows on enter and keeps showing while the drag moves over the window", () => {
    assert.equal(nextDropOverlayVisible(false, "enter"), true);
    assert.equal(nextDropOverlayVisible(true, "over"), true);
    assert.equal(nextDropOverlayVisible(false, "over"), true);
  });

  it("hides when the drag leaves the window", () => {
    assert.equal(nextDropOverlayVisible(true, "leave"), false);
  });

  it("hides on drop, so a completed drop never leaves the overlay up", () => {
    assert.equal(nextDropOverlayVisible(true, "drop"), false);
  });

  it("a new enter after leave shows again", () => {
    let visible = false;
    for (const phase of ["enter", "over", "leave", "enter", "drop"] as const) {
      visible = nextDropOverlayVisible(visible, phase);
    }
    assert.equal(visible, false);
    visible = nextDropOverlayVisible(visible, "enter");
    assert.equal(visible, true);
  });
});
