// The credential-free half of GATE-CONTRACT NN31 in publish-direct.yml: the gate admission job names the sealed gate archive through
// reviewed-candidate.mjs, and the sealed bytes must still match the manifest's line for them.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { exportGateArchive, gateArchiveOfPack } from "./reviewed-candidate.mjs";

const sha = (b) => createHash("sha256").update(b).digest("hex");
const commit = "6d6bc1bc8a89f0a762000be18695df3a2dec7af2";
function pack(t, units) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-export-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const listed = units.map(({ name, file, bytes, sha256 }) => { fs.writeFileSync(path.join(dir, file), bytes); return { name, version: "0.6.58", file, sha256: sha256 ?? sha(bytes) }; });
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify({ schema: 1, registry: "npm", commit, units: listed }));
  return dir;
}

test("the gate archive of a pack is named with its sealed hash and commit; a pack without the gate package names none", (t) => {
  const bytes = Buffer.from("gate archive");
  const dir = pack(t, [{ name: "@rightkit/theme", file: "theme.tgz", bytes: Buffer.from("t") }, { name: "@rightkit/release", file: "release.tgz", bytes }]);
  assert.deepEqual(gateArchiveOfPack({ packDir: dir }), { name: "@rightkit/release", version: "0.6.58", file: path.join(dir, "release.tgz"), sha256: sha(bytes), commit });
  assert.equal(gateArchiveOfPack({ packDir: pack(t, [{ name: "@rightkit/theme", file: "theme.tgz", bytes: Buffer.from("t") }]) }), null);
});

test("an archive that differs from its manifest line, or a line that could name another file, is refused", (t) => {
  assert.throws(() => gateArchiveOfPack({ packDir: pack(t, [{ name: "@rightkit/release", file: "release.tgz", bytes: Buffer.from("x"), sha256: "0".repeat(64) }]) }), /differs from its manifest line/);
  const dir = pack(t, [{ name: "@rightkit/release", file: "release.tgz", bytes: Buffer.from("x") }]);
  const plan = JSON.parse(fs.readFileSync(path.join(dir, "manifest.json"), "utf8")); plan.units[0].file = "../release.tgz";
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify(plan));
  assert.throws(() => gateArchiveOfPack({ packDir: dir }), /malformed/);
});

test("the export step refuses to start where a registry token is present", async () => {
  for (const key of ["NODE_AUTH_TOKEN", "NPM_TOKEN", "CARGO_REGISTRY_TOKEN"]) {
    await assert.rejects(exportGateArchive({ env: { [key]: "x" }, fetchImpl: () => { throw new Error("no network"); } }), new RegExp(`${key} is present`));
  }
});
