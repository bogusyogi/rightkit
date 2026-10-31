// GB-49: every reader and digest of the declared compile inputs reproduces RustRight's vector (test-support/vectors/declared-inputs.fixtures.json: the digests were
// computed independently of the broker): canonical declaration first, legacy only when it is absent, an invalid canonical one is an error (never a fallback),
// links/absolute/`..`/overlap refused, names joined with `/` on every platform, non-ASCII names sorted by their bytes.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { judgeCompileInputs } from "./compile-inputs.mjs";
import { assertSuppliedInputs, declaredInputDigestsOf, readDeclaredInputs } from "./snapshot-request.mjs";
import { loadVector } from "./test-support/vectors.mjs";

const vector = loadVector("declared-inputs.fixtures.json");
const build = (t, c) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-decl-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [rel, body] of [...Object.entries(c.files), ...Object.entries(c.declarations)]) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); }
  return root;
};

test("[GB-49] every case of the declared-inputs vector: the gate's readers, supplied-input check and digests give RustRight's inputs, digests, counts and refusals", (t) => {
  for (const c of vector.cases) {
    const root = build(t, c);
    if (c.expect.error) {
      const outcome = (() => { try { const inputs = assertSuppliedInputs(root, c.supplied); declaredInputDigestsOf(root, inputs); return null; } catch (e) { return e.message; } })();
      assert.ok(outcome?.includes(c.expect.error), `${c.name}: ${outcome}`);
      // The captured-tree judge is the same reader: it reports the same refusal as a finding (unless the failure is only the supplied comparison).
      if (c.expect.error !== "declaredInputs differs from the snapshot declaration") assert.ok(judgeCompileInputs(root).findings.join("\n").includes(c.expect.error), `${c.name}: judge`);
      continue;
    }
    const inputs = assertSuppliedInputs(root, c.supplied);
    assert.deepEqual(declaredInputDigestsOf(root, inputs), c.expect.declared, c.name);
    const judged = judgeCompileInputs(root);
    assert.deepEqual(judged.findings, [], c.name);
    assert.deepEqual(judged.inputs.map(({ path: p, sha256 }) => ({ path: p, sha256 })), c.expect.declared, `${c.name}: the captured-tree judge digests the same bytes`);
    assert.equal(judged.inputs.reduce((n, i) => n + i.files, 0), c.expect.files, `${c.name}: files`);
  }
});

test("[GB-49] the digest names a file with `/` whatever separator the declaration used, so a Windows host and a POSIX host agree", (t) => {
  const root = build(t, { files: { "data/sub/b.bin": "yz", "data/sub/c.bin": "w" }, declarations: {} });
  const slash = declaredInputDigestsOf(root, ["data/sub"])[0].sha256;
  assert.equal(declaredInputDigestsOf(root, ["data\\sub"])[0].sha256, slash);
  assert.equal(slash, "cbdb52cdeaa5b854b302904f75483bc4b5cf70311b804697aa945f8eae994ec5");
  // The platform is no input of the digest any more.
  assert.equal(declaredInputDigestsOf(root, ["data/sub"], { platform: "win32" })[0].sha256, slash);
});

test("[GB-49] a link on a declaration path or inside an input is refused; a canonical declaration that is a link is refused and not read as legacy", (t) => {
  if (process.platform === "win32") return t.skip("symlink creation needs privileges");
  const root = build(t, { files: { "real/m.json": '{"inputs":["a.txt"]}', "a.txt": "1" }, declarations: { ".rightkit/compile-inputs.json": '{"inputs":["a.txt"]}' } });
  fs.mkdirSync(path.join(root, ".rustright"));
  fs.symlinkSync(path.join(root, "real", "m.json"), path.join(root, ".rustright", "compile-inputs.json"));
  assert.throws(() => readDeclaredInputs(root), /runs through a link/);
  fs.rmSync(path.join(root, ".rustright"), { recursive: true });
  assert.deepEqual(readDeclaredInputs(root), ["a.txt"], "with no canonical declaration the legacy one is read");
  fs.mkdirSync(path.join(root, "d")); fs.symlinkSync(path.join(root, "a.txt"), path.join(root, "d", "l"));
  assert.throws(() => declaredInputDigestsOf(root, ["d"]), /holds a link/);
});
