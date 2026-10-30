import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { judgeCompileInputs } from "./compile-inputs.mjs";
import { main } from "./bin/rightkit-gate.mjs";

function captured(t, files, declared = ["dist"]) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-compile-inputs-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  if (declared) {
    fs.mkdirSync(path.join(root, ".rightkit"), { recursive: true });
    fs.writeFileSync(path.join(root, ".rightkit/compile-inputs.json"), JSON.stringify({ inputs: declared }));
  }
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
    fs.writeFileSync(path.join(root, name), text);
  }
  return root;
}

// The broker's digest: per file in name order, name NUL bytes NUL.
const brokerDigest = (entries) => {
  const h = crypto.createHash("sha256");
  for (const [name, text] of entries) { h.update(name); h.update(Buffer.from([0])); h.update(text); h.update(Buffer.from([0])); }
  return h.digest("hex");
};

test("valid declared inputs pass, and the digest is the broker's for the same bytes", (t) => {
  const root = captured(t, { "dist/app.js": "export const a = 1;\n", "dist/b/data.json": "{\"x\":1}\n", "dist/a.txt": "plain\n" });
  const judged = judgeCompileInputs(root);
  assert.deepEqual(judged.findings, []);
  assert.deepEqual(judged.inputs, [{ path: "dist", files: 3, sha256: brokerDigest([["dist/a.txt", "plain\n"], ["dist/app.js", "export const a = 1;\n"], ["dist/b/data.json", "{\"x\":1}\n"]]) }]);
});

test("each kind of bad declared input is a finding: syntax, JSON, secret, missing, link, unsafe path", (t) => {
  const findings = (files, declared) => judgeCompileInputs(captured(t, files, declared)).findings.join("\n");
  assert.match(findings({ "dist/app.js": "function (\n" }), /dist\/app\.js: does not parse/);
  assert.match(findings({ "dist/x.mjs": "export default {\n" }), /dist\/x\.mjs: does not parse/);
  assert.match(findings({ "dist/x.json": "{ nope" }), /dist\/x\.json: invalid JSON/);
  assert.match(findings({ "dist/env.js": `const t = "ghp_${"a".repeat(36)}";\n` }), /dist\/env\.js: holds a secret/);
  assert.match(findings({}, ["dist"]), /dist: missing from the captured tree/);
  assert.match(findings({}, ["../outside"]), /not a plain relative path/);
  const root = captured(t, { "elsewhere.js": "1;\n" });
  fs.mkdirSync(path.join(root, "dist"));
  fs.symlinkSync(path.join(root, "elsewhere.js"), path.join(root, "dist/app.js"));
  assert.match(judgeCompileInputs(root).findings.join("\n"), /symbolic link at dist\/app\.js/);
  assert.match(judgeCompileInputs(captured(t, {}, null)).findings.join("\n"), /^$/, "no declaration, nothing to judge");
});

test("rightkit-gate check judges the captured declared inputs named by RIGHTKIT_COMPILE_INPUTS (red stays red, repaired passes)", async (t) => {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "rk-compile-inputs-repo-"));
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(repo, "notes.txt"), "app\n"); git("add", "-A"); git("commit", "-q", "-m", "app");
  const run = async (files) => {
    const root = captured(t, files);
    const out = []; const err = [];
    const code = await main(["check", "--commit", "HEAD", "--repo", repo], { out: (s) => out.push(s), err: (s) => err.push(s), cwd: repo, appCheck: async () => 0, env: { ...process.env, GITHUB_ACTIONS: "true", RIGHTKIT_COMPILE_INPUTS: root }, classA: undefined, runClassA: undefined });
    return { code, text: [...out, ...err].join("\n") };
  };
  const red = await run({ "dist/app.js": "function (\n" });
  assert.equal(red.code, 1, red.text);
  assert.match(red.text, /compile input FAIL dist\/app\.js: does not parse/);
  const green = await run({ "dist/app.js": "export const a = 1;\n" });
  assert.equal(green.code, 0, green.text);
  assert.match(green.text, new RegExp(`compile input dist: 1 file\\(s\\), sha256 ${brokerDigest([["dist/app.js", "export const a = 1;\n"]])}`));
});

test("[B2] judgeCompileInputs reads `.rustright/compile-inputs.json` first, the legacy `.rightkit` one only when it is absent, and guards the overlap of both", (t) => {
  const root = captured(t, { "a/x.txt": "ax", "b/y.txt": "by" }, null);
  const decl = (rel, inputs) => { fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); fs.writeFileSync(path.join(root, rel), JSON.stringify({ inputs })); };
  decl(".rustright/compile-inputs.json", ["a"]);
  assert.deepEqual(judgeCompileInputs(root).inputs.map((i) => i.path), ["a"], "only .rustright");
  decl(".rightkit/compile-inputs.json", ["b"]);
  const both = judgeCompileInputs(root);
  assert.deepEqual(both.inputs.map((i) => i.path), ["a"], "both files: .rustright wins");
  assert.deepEqual(both.findings, []);
  fs.rmSync(path.join(root, ".rustright"), { recursive: true });
  assert.deepEqual(judgeCompileInputs(root).inputs.map((i) => i.path), ["b"], "legacy only when .rustright is absent");
  fs.rmSync(path.join(root, ".rightkit"), { recursive: true });
  assert.deepEqual(judgeCompileInputs(root), { inputs: [], findings: [] }, "neither: no declared inputs");
  fs.mkdirSync(path.join(root, ".rustright"), { recursive: true }); fs.writeFileSync(path.join(root, ".rustright/compile-inputs.json"), "{ nope");
  const bad = judgeCompileInputs(root);
  assert.equal(bad.inputs.length, 0); assert.match(bad.findings[0], /^\.rustright\/compile-inputs\.json: /, "callers filter malformed declarations on this prefix");
  fs.rmSync(path.join(root, ".rustright"), { recursive: true });
  fs.mkdirSync(path.join(root, ".rightkit"), { recursive: true }); fs.writeFileSync(path.join(root, ".rightkit/compile-inputs.json"), "{ nope");
  assert.match(judgeCompileInputs(root).findings[0], /^\.rustright\/compile-inputs\.json: \.rightkit\/compile-inputs\.json: /, "a malformed legacy declaration keeps the same prefix and names its file");
  fs.rmSync(path.join(root, ".rightkit"), { recursive: true });
  for (const input of [".rustright", ".rustright/compile-inputs.json", ".rightkit", ".rightkit/compile-inputs.json"]) {
    decl(".rustright/compile-inputs.json", [input]);
    assert.match(judgeCompileInputs(root).findings.join("\n"), /declaration file itself/, input);
  }
});

test("[M23] the declared-input digest joins child names with the host's separator: a Windows host hashes `\\`, and it equals the snapshot-request digest for that platform and differs from the POSIX one", (t) => {
  const root = captured(t, { "dist/a.txt": "A", "dist/sub/c.txt": "C" }, ["dist"]);
  const posix = judgeCompileInputs(root, { platform: "darwin" }).inputs[0].sha256;
  const win = judgeCompileInputs(root, { platform: "win32" }).inputs[0].sha256;
  assert.equal(posix, brokerDigest([["dist/a.txt", "A"], ["dist/sub/c.txt", "C"]]));
  assert.equal(win, brokerDigest([["dist\\a.txt", "A"], ["dist\\sub\\c.txt", "C"]]));
  assert.notEqual(win, posix);
  assert.equal(win, declaredInputDigestsOf(root, ["dist"], { platform: "win32" })[0].sha256);
  assert.equal(posix, declaredInputDigestsOf(root, ["dist"], { platform: "darwin" })[0].sha256);
});
import { declaredInputDigestsOf } from "./snapshot-request.mjs";
