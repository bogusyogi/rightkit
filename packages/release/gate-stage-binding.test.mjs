import assert from "node:assert/strict";
import test from "node:test";
import { bindingOf, sameBinding, sameSource } from "./gate-stages.mjs";

test("#1550: an imported judge change invalidates stage reuse even with identical entrypoint bytes", () => {
  const inputs = { tree: "tree", gateFile: "entry.mjs", readFile: () => Buffer.from("unchanged entrypoint"), executor: "broker" };
  const first = bindingOf({ ...inputs, gateJudge: "seam-before" });
  const warm = bindingOf({ ...inputs, gateJudge: "seam-before" });
  const changed = bindingOf({ ...inputs, gateJudge: "seam-after" });
  const legacy = bindingOf(inputs);
  assert.equal(sameBinding(first, warm), true);
  assert.equal(sameSource(first, warm), true);
  for (const stale of [changed, legacy]) {
    assert.equal(sameBinding(first, stale), false);
    assert.equal(sameSource(first, stale), false);
  }
});
