// The declared compile inputs of a gate snapshot (reset plan step 5: the broker captures none, the gate judges them in the snapshot).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { COMPILE_INPUTS_CANONICAL, COMPILE_INPUTS_LEGACY, judgeCompileInputs, readDeclaredInputs } from "./compile-inputs.mjs";

const tree = (files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-inputs-"));
  for (const [name, text] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true }); fs.writeFileSync(path.join(root, name), text); }
  return root;
};

test("the canonical declaration wins; the legacy one is read only when it is absent; an invalid canonical one never falls back", () => {
  assert.deepEqual(readDeclaredInputs(tree({ [COMPILE_INPUTS_CANONICAL]: '{"inputs":["a"]}', [COMPILE_INPUTS_LEGACY]: '{"inputs":["b"]}' })), ["a"]);
  assert.deepEqual(readDeclaredInputs(tree({ [COMPILE_INPUTS_LEGACY]: '{"inputs":["b"]}' })), ["b"]);
  assert.deepEqual(readDeclaredInputs(tree({})), []);
  assert.throws(() => readDeclaredInputs(tree({ [COMPILE_INPUTS_CANONICAL]: "{not json", [COMPILE_INPUTS_LEGACY]: '{"inputs":["b"]}' })), /compile-inputs\.json/);
  assert.throws(() => readDeclaredInputs(tree({ [COMPILE_INPUTS_CANONICAL]: '{"inputs":["../x"]}' })), /plain relative path/);
});

test("a platform-scoped input is declared only on the hosts it names", () => {
  const root = tree({ [COMPILE_INPUTS_CANONICAL]: '{"inputs":["a",{"path":"w","platforms":["win"]}]}' });
  assert.deepEqual(readDeclaredInputs(root, { platform: "darwin" }), ["a"]);
  assert.deepEqual(readDeclaredInputs(root, { platform: "win32" }), ["a", "w"]);
});

test("judgeCompileInputs digests the declared bytes and separates a missing path from a content finding and from a malformed declaration", () => {
  const ok = tree({ [COMPILE_INPUTS_CANONICAL]: '{"inputs":["dist"]}', "dist/app.json": '{"a":1}' });
  const judged = judgeCompileInputs(ok);
  assert.equal(judged.inputs.length, 1); assert.equal(judged.inputs[0].files, 1); assert.match(judged.inputs[0].sha256, /^[0-9a-f]{64}$/);
  assert.deepEqual(judged.findings, []); assert.equal(judged.malformed, null);

  const missing = judgeCompileInputs(tree({ [COMPILE_INPUTS_CANONICAL]: '{"inputs":["dist"]}' }));
  assert.equal(missing.findings.length, 1); assert.deepEqual(missing.missing, missing.findings, "a missing path is a note, not a refusal");

  const bad = judgeCompileInputs(tree({ [COMPILE_INPUTS_CANONICAL]: '{"inputs":["dist"]}', "dist/app.json": "{nope" }));
  assert.equal(bad.findings.length, 1); assert.match(bad.findings[0], /invalid JSON/); assert.deepEqual(bad.missing, [], "a file that does not parse is a refusal");

  const malformed = judgeCompileInputs(tree({ [COMPILE_INPUTS_LEGACY]: "{not json" }));
  assert.match(malformed.malformed, /compile-inputs\.json/); assert.deepEqual(malformed.inputs, []);
});
