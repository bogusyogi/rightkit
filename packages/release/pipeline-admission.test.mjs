import assert from "node:assert/strict";
import test from "node:test";
import { admitHookString, admitInvocation } from "./pipeline-admission.mjs";

const admitted = (text, scripts) => admitHookString(text, { scripts });

test("rightkit-gate install-hook is an admitted hook command, bare or through pnpm exec (an app's prepare script)", () => {
  assert.equal(admitted("rightkit-gate install-hook").admitted, true);
  assert.equal(admitted("pnpm exec rightkit-gate install-hook").admitted, true);
  assert.equal(admitted("rightkit-gate --version").admitted, true);
  assert.equal(admitInvocation({ cmd: "rightkit-gate", args: ["install-hook"] }).admitted, true);
  // prepare is an npm lifecycle hook: the alias route admits it too.
  assert.equal(admitted("pnpm run prepare", { prepare: "rightkit-gate install-hook" }).admitted, true);
});

test("rightkit-gate is admitted only as the installer: the gate, extra flags and chains stay refused", () => {
  for (const text of ["rightkit-gate", "rightkit-gate --base origin/main", "rightkit-gate install-hook --force", "rightkit-gate check", "pnpm exec rightkit-gate --plan", "pnpm exec rightkit-gate install-hook --force"]) {
    const result = admitted(text);
    assert.equal(result.admitted, false, text);
    assert.equal(result.code, "R1-owner-subcommand", text);
  }
  assert.equal(admitted("rightkit-gate install-hook && echo done").admitted, false);
  assert.equal(admitted("rightkit-gate install-hook; rm -rf .").admitted, false);
});
