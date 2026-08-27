import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

import { verifyStandaloneClones } from "./standalone-clone-verify.mjs";

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, `${command} ${args.join(" ")}\n${result.stderr}`);
}

test("standalone verifier clones, installs, doctors from a nested app root, and removes only its own temp tree", async (t) => {
  const fixtureRoot = mkdtempSync(path.join(tmpdir(), "rightkit-standalone-fixture-"));
  t.after(() => rmSync(fixtureRoot, { recursive: true, force: true }));
  const source = path.join(fixtureRoot, "source");
  const outsideForge = path.join(fixtureRoot, "keep.txt");
  mkdirSync(path.join(source, "apps", "desktop"), { recursive: true });
  writeFileSync(outsideForge, "keep", "utf8");
  writeFileSync(path.join(source, "apps", "desktop", "package.json"), JSON.stringify({
    name: "standalone-fixture",
    private: true,
    packageManager: "pnpm@11.24.0",
    scripts: { "release:doctor": "node doctor.mjs" },
  }), "utf8");
  writeFileSync(path.join(source, "apps", "desktop", "pnpm-lock.yaml"), "lockfileVersion: '9.0'\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\nimporters:\n  .: {}\n", "utf8");
  writeFileSync(path.join(source, "apps", "desktop", "doctor.mjs"), "process.stdout.write('fixture doctor passed\\n')\n", "utf8");
  run("git", ["init", "-q"], source);
  run("git", ["config", "user.email", "fixture@example.test"], source);
  run("git", ["config", "user.name", "Fixture"], source);
  run("git", ["add", "."], source);
  run("git", ["commit", "-qm", "fixture"], source);

  const evidencePath = path.join(fixtureRoot, "evidence.json");
  const result = await verifyStandaloneClones({
    apps: [{ key: "fixture", remote: source, appDir: "apps/desktop" }],
    evidencePath,
    tempParent: fixtureRoot,
  });

  assert.equal(result.apps[0].install.status, 0);
  assert.equal(result.apps[0].doctor.status, 0);
  assert.match(result.apps[0].doctor.stdout, /fixture doctor passed/);
  assert.equal(existsSync(result.workRoot), false, "generated clone tree must be removed");
  assert.equal(readFileSync(outsideForge, "utf8"), "keep", "cleanup must not escape the generated tree");
  assert.deepEqual(JSON.parse(readFileSync(evidencePath, "utf8")).apps.map(({ key }) => key), ["fixture"]);
});

test("release package exposes the local standalone verification lane", () => {
  const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
  assert.equal(pkg.scripts["verify:standalone"], "node standalone-clone-verify.mjs");
});

test("standalone verifier rejects a package root that escapes its clone", async (t) => {
  const fixtureRoot = mkdtempSync(path.join(tmpdir(), "rightkit-standalone-escape-"));
  t.after(() => rmSync(fixtureRoot, { recursive: true, force: true }));
  const source = path.join(fixtureRoot, "source");
  mkdirSync(source, { recursive: true });
  writeFileSync(path.join(source, "README.md"), "fixture", "utf8");
  run("git", ["init", "-q"], source);
  run("git", ["config", "user.email", "fixture@example.test"], source);
  run("git", ["config", "user.name", "Fixture"], source);
  run("git", ["add", "."], source);
  run("git", ["commit", "-qm", "fixture"], source);

  await assert.rejects(
    verifyStandaloneClones({
      apps: [{ key: "fixture", remote: source, appDir: ".." }],
      evidencePath: path.join(fixtureRoot, "evidence.json"),
      tempParent: fixtureRoot,
    }),
    /package root escapes its clone/,
  );
});
