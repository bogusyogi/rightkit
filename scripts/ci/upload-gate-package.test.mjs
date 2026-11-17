import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { executeUpload, GATE_ADMISSION_RECORD_ENV, GATE_ADMISSION_RECORD_SCHEMA } from "./upload.mjs";

const sha = (b) => createHash("sha256").update(b).digest("hex");
const commit = "6d6bc1bc8a89f0a762000be18695df3a2dec7af2";
// A sealed pack as publish.yml writes it, for one npm unit.
const pack = (name, version = "0.6.58") => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-upload-"));
  const file = `${name.replace(/^@/, "").replace("/", "-")}-${version}.tgz`; const bytes = Buffer.from(`archive of ${name}@${version}`);
  fs.writeFileSync(path.join(dir, file), bytes);
  const unit = { name, version, file, sha256: sha(bytes), size: bytes.length, integrity: "sha512-x", sha1: "y" };
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify({ schema: 1, registry: "npm", repository: "bogusyogi/rightkit", runId: "44124086166", runAttempt: 1, commit, units: [unit] }));
  return { dir, unit };
};
const env = (extra = {}) => ({ GITHUB_ACTIONS: "true", GITHUB_REF: "refs/heads/main", GITHUB_REPOSITORY: "bogusyogi/rightkit", PUBLISH_REGISTRY: "npm", GITHUB_RUN_ID: "44124086166", GITHUB_RUN_ATTEMPT: "1", GITHUB_SHA: commit, ...extra });
const run = async (p, e, spawned) => {
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "rk-upload-out-"));
  try { await executeUpload({ env: e, packDir: p.dir, receiptsDir: p.dir, approvedFile: path.join(p.dir, "none.json"), outDir: out, dryRun: true, spawn: (cmd, args, o) => { spawned.push([cmd, ...(args ?? [])].join(" ")); return { status: 0, stdout: "", stderr: "" }; }, fetchImpl: async () => { throw new Error("no network in this test"); }, log: () => {} }); return null; }
  catch (e) { return e; }
};

test("the upload refuses the gate package before anything else when no admission record is named", async () => {
  const p = pack("@rightkit/release"); const spawned = [];
  const e = await run(p, env({ PUBLISH_UNITS: "@rightkit/release" }), spawned);
  assert.ok(e, "it must refuse");
  assert.match(e.message, /is not uploaded without a passing contract-test admission/);
  assert.match(e.message, new RegExp(GATE_ADMISSION_RECORD_ENV));
  assert.deepEqual(spawned, [], "no process was started: no npm, no pack check");
});

test("a record for another archive, version or commit is refused the same way; any other package is not stopped by this check", async () => {
  const p = pack("@rightkit/release");
  const write = (rec) => { const f = path.join(p.dir, `rec-${Math.random()}.json`); fs.writeFileSync(f, JSON.stringify({ schema: GATE_ADMISSION_RECORD_SCHEMA, ok: true, unit: "@rightkit/release", version: "0.6.58", archiveSha256: p.unit.sha256, commit, ...rec })); return f; };
  for (const [why, rec, re] of [["other archive", { archiveSha256: "0".repeat(64) }, /for the archive/], ["other version", { version: "0.6.57" }, /@0\.6\.57/], ["other commit", { commit: "a".repeat(40) }, /for commit/], ["not passing", { ok: false }, /not a passing/]]) {
    const spawned = []; const e = await run(p, env({ PUBLISH_UNITS: "@rightkit/release", [GATE_ADMISSION_RECORD_ENV]: write(rec) }), spawned);
    assert.match(e?.message ?? "", re, why); assert.deepEqual(spawned, [], why);
  }
  // With the right record the gate check lets it through to the run's other checks, which fail here for want of verify receipts: a different refusal.
  const ok = await run(p, env({ PUBLISH_UNITS: "@rightkit/release", [GATE_ADMISSION_RECORD_ENV]: write({}) }), []);
  assert.ok(ok, "the other checks still apply"); assert.doesNotMatch(ok.message, /contract-test admission/);
  // Another package never needs the record.
  const other = await run(pack("@rightkit/theme", "0.3.1"), env({ PUBLISH_UNITS: "@rightkit/theme" }), []);
  assert.ok(other); assert.doesNotMatch(other.message, /contract-test admission/);
});
