// The host launcher (packages/build-control-policy/src/host-gate.mjs) had no test. It decides which installed gate every app build on a
// Mac or Windows host runs. These tests build sealed installs on disk the way `rustright-install trusted-gate` leaves them: a folder
// named by the sha256 of `<name>.manifest.txt`, each manifest line `./path<TAB>file<TAB>-<TAB>sha256`.
import { test } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { missingModules, newestInstalledGate, trustedGate } from "../build-control-policy/src/host-gate.mjs";

const sha = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const PKG = "node_modules/@rightkit/release";

/** Writes one sealed install under `root` and returns its folder name. `files`: { "bin/rightkit-gate.mjs": text, ... } relative to the package. */
function install(root, version, files, { salt = "" } = {}) {
  const all = { "package.json": JSON.stringify({ name: "@rightkit/release", version }), ...files };
  const lines = Object.entries(all).sort(([a], [b]) => (a < b ? -1 : 1)).map(([rel, text]) => `./${PKG}/${rel}\tfile\t-\t${sha(text)}`);
  if (salt) lines.push(`./.modules.yaml\tfile\t-\t${sha(salt)}`);
  const manifest = `${lines.join("\n")}\n`;
  const name = sha(manifest);
  for (const [rel, text] of Object.entries(all)) { const f = path.join(root, name, PKG, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, text); }
  if (salt) fs.writeFileSync(path.join(root, name, ".modules.yaml"), salt);
  fs.writeFileSync(path.join(root, `${name}.manifest.txt`), manifest);
  return name;
}
const ENTRY = 'import "../seam.mjs";\nexport const run = async () => (await import("../gate/compile.mjs")).compile();\n';
const COMPLETE = { "bin/rightkit-gate.mjs": ENTRY, "seam.mjs": "export const seam = 1;\n", "gate/compile.mjs": "export const compile = () => 0;\n" };
const WITHOUT_GATE = { "bin/rightkit-gate.mjs": ENTRY, "seam.mjs": "export const seam = 1;\n" };
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "host-gate-"));
const pick = (root) => newestInstalledGate({ root });

test("the newest sealed install is chosen; with none the answer is null", () => {
  const root = tmp();
  assert.equal(pick(root), null);
  install(root, "0.6.56", COMPLETE);
  const newest = install(root, "0.6.58", COMPLETE);
  install(root, "0.6.9", COMPLETE); // 0.6.9 is older than 0.6.56: versions compare by number, not by text
  const got = pick(root);
  assert.equal(got.version, "0.6.58"); assert.equal(got.install, newest); assert.deepEqual(got.missing, []); assert.deepEqual(got.skipped, []);
});

test("an install whose entry loads a module the package lacks still runs, and is named INCOMPLETE at every launch (0.6.57 had no gate/ folder)", () => {
  const root = tmp();
  install(root, "0.6.56", COMPLETE);
  const broken = install(root, "0.6.57", WITHOUT_GATE);
  const got = pick(root);
  assert.equal(got.version, "0.6.57", "the newest stays the one that runs: the version before it may be one the broker refuses");
  assert.equal(got.install, broken); assert.deepEqual(got.missing, ["gate/compile.mjs"]);
  const lines = [];
  const entry = trustedGate({ root, warn: (line) => lines.push(line) });
  assert.ok(entry.endsWith(path.join("bin", "rightkit-gate.mjs")));
  assert.equal(lines.length, 1); assert.match(lines[0], /installed gate 0\.6\.57 is INCOMPLETE: it loads gate\/compile\.mjs, which its package does not contain/);
  // A complete newer install ends the warning.
  install(root, "0.6.58", COMPLETE);
  const after = []; trustedGate({ root, warn: (line) => after.push(line) });
  assert.deepEqual(after, []);
});

test("an install with a changed file is passed over, said aloud, and the next one runs; when none is intact the launcher refuses", () => {
  const root = tmp();
  const old = install(root, "0.6.56", COMPLETE);
  const newest = install(root, "0.6.58", COMPLETE);
  fs.writeFileSync(path.join(root, newest, PKG, "seam.mjs"), "export const seam = 2; // edited after install\n");
  const got = pick(root);
  assert.equal(got.version, "0.6.56"); assert.equal(got.install, old);
  assert.deepEqual(got.skipped.map((s) => [s.version, s.reason]), [["0.6.58", "a file no longer matches its manifest"]]);
  const lines = []; trustedGate({ root, warn: (line) => lines.push(line) });
  assert.match(lines[0], /installed gate 0\.6\.58 \([0-9a-f]{12}\) is not used: a file no longer matches its manifest/);
  fs.writeFileSync(path.join(root, old, PKG, "seam.mjs"), "tampered\n");
  assert.throws(() => trustedGate({ root, warn: () => {} }), /no installed gate is intact \(2 install\(s\) passed over/);
});

test("two installs of one version: the choice is the same whatever order the directory lists them in", () => {
  const root = tmp();
  const a = install(root, "0.6.58", COMPLETE, { salt: "first install" });
  const b = install(root, "0.6.58", COMPLETE, { salt: "second install" });
  assert.notEqual(a, b);
  const expected = [a, b].sort()[0];
  const names = fs.readdirSync(root);
  for (const order of [names, [...names].reverse()]) assert.equal(newestInstalledGate({ root, list: () => order }).install, expected);
});

test("missingModules follows static and import() specifiers that name an extension, and nothing else", () => {
  const files = new Map(Object.keys({ ...COMPLETE, "package.json": "" }).map((rel) => [`./${PKG}/${rel}`, "x"]));
  const text = (rel) => ({ ...COMPLETE, "bin/rightkit-gate.mjs": `${ENTRY}// prose: from "./x" and import("../other")\nimport "../../outside/pkg.mjs";\n` })[rel.slice(`./${PKG}/`.length)];
  assert.deepEqual(missingModules(files, text), [], "a specifier without an extension is prose; a path that leaves the package is another package's");
  files.delete(`./${PKG}/seam.mjs`);
  assert.deepEqual(missingModules(files, text), ["seam.mjs"]);
});
