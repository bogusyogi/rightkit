import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { syncCommand } from "./sync.mjs";
import { installCommand } from "./install.mjs";
import { uninstallCommand } from "./uninstall.mjs";
import { listManagedFiles } from "./sync.mjs";
import { MANIFEST_FILENAME } from "./manifest.mjs";
import { makeTempRepo, PUBLIC_DEPS, SAMPLE_MANIFEST } from "./test-support.mjs";

test("uninstall removes every managed file, leaving zero behind — including the manifest (F6/I5)", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(listManagedFiles(dir).length, 2);

  const result = uninstallCommand(dir);
  assert.equal(result.ok, true);
  // 2 workflow files + the manifest itself: I5 requires *zero* managed
  // files left behind, and .rightgit.json is a file `install` wrote.
  assert.equal(result.removed.length, 3);
  assert.ok(result.removed.includes(MANIFEST_FILENAME));
  assert.deepEqual(listManagedFiles(dir), []);
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows", "ci.yml")), false);
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows", "package-smoke.yml")), false);
  assert.equal(fs.existsSync(path.join(dir, MANIFEST_FILENAME)), false);
});

// F6 regression: install writes .rightgit.json *before* running sync (see
// install.mjs), so a repo whose visibility check flips or errors between
// that write and sync's own visibility re-check is left with an orphaned
// manifest and zero (or partial) workflow files. The old uninstallCommand
// only ever looked at marker-bearing workflow files, so it reported success
// while leaving the manifest behind. Reproduces the QA report's
// "interruptedInstall" probe.
test("F6: uninstall removes the manifest left behind by an install interrupted after the visibility check", async () => {
  const dir = makeTempRepo(null);
  let calls = 0;
  const flakyDeps = {
    execFile: async (cmd) => {
      calls += 1;
      if (cmd === "gh" && calls === 1) return { stdout: JSON.stringify({ visibility: "PUBLIC" }) };
      if (cmd === "gh") throw new Error("gh: rate limited");
      throw new Error(`unexpected exec: ${cmd}`);
    },
  };

  const installResult = await installCommand(dir, { lanes: ["ci"] }, flakyDeps);
  assert.equal(installResult.ok, false);
  assert.equal(installResult.stage, "synced");
  assert.ok(fs.existsSync(path.join(dir, MANIFEST_FILENAME)), "setup: interrupted install must still leave the manifest on disk");

  const uninstallResult = uninstallCommand(dir);
  assert.equal(uninstallResult.ok, true);
  assert.ok(uninstallResult.removed.includes(MANIFEST_FILENAME));
  assert.equal(fs.existsSync(path.join(dir, MANIFEST_FILENAME)), false, "F6: manifest must not survive uninstall after an interrupted install");
});

test("uninstall never touches an unmanaged file in the same directory", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const handWritten = path.join(dir, ".github", "workflows", "hand-written.yml");
  fs.writeFileSync(handWritten, "name: hand-written\n", "utf8");

  uninstallCommand(dir);
  assert.ok(fs.existsSync(handWritten), "uninstall deleted a file it does not manage");
});

test("uninstall on a repo with no managed files is a no-op, not an error", () => {
  // makeTempRepo(null): no manifest, no workflow files — genuinely zero
  // managed state (makeTempRepo(SAMPLE_MANIFEST) would itself write
  // .rightgit.json, which is now correctly treated as managed per F6).
  const dir = makeTempRepo(null);
  const result = uninstallCommand(dir);
  assert.equal(result.ok, true);
  assert.deepEqual(result.removed, []);
});
