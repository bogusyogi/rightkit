import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { execFileSync } from "./background-process.mjs";
import { DEFAULT_STORAGE_THRESHOLD_BYTES, findInstallerCopyScripts, findLargeRepositorySubdirectories } from "./installer-storage-doctor.mjs";

test("doctor reports installer-copy scripts without blocking", () => {
  const root = mkdtempSync(path.join("/private/tmp", "rightkit-installer-doctor-"));
  try {
    writeFileSync(path.join(root, "package.json"), JSON.stringify({ scripts: { package: "cp target/App.dmg ." } }));
    const known = path.join(root, "package.sh");
    writeFileSync(known, "cp target/App.exe .");
    const findings = findInstallerCopyScripts({ appRoot: root, workspaceRoot: root, knownPaths: ["package.sh"] });
    assert.equal(findings.length, 2);
    assert.match(findings[0].reason, /copies/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("doctor matches installer destination while ignoring helper and managed copies", () => {
  const root = mkdtempSync(path.join("/private/tmp", "rightkit-installer-doctor-"));
  try {
    writeFileSync(path.join(root, "package.sh"), [
      'cp "$HELPER_DYLIB" "$STAGE/"',
      'cp "$DMG_PATH" "$REPO_ROOT/"',
      'cp "$ROOT_INSTALLER" "$RIGHT_RELEASE_ARTIFACT_DIR/"',
    ].join("\n"));
    const findings = findInstallerCopyScripts({ appRoot: root, workspaceRoot: root, knownPaths: [] });
    assert.equal(findings.length, 1);
    assert.equal(findings[0].line, 2);
    assert.equal(findings[0].destination, '"$REPO_ROOT/"');
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("known app script is scoped to selected app root", () => {
  const root = mkdtempSync(path.join("/private/tmp", "rightkit-installer-doctor-"));
  try {
    mkdirSync(path.join(root, "scraperight"), { recursive: true });
    mkdirSync(path.join(root, "other"), { recursive: true });
    writeFileSync(path.join(root, "scraperight", "package.sh"), 'cp "$DMG_PATH" "$REPO_ROOT/"');
    assert.deepEqual(findInstallerCopyScripts({ appRoot: path.join(root, "other"), workspaceRoot: root }), []);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("ownership scanner reports large repository directories and ignores managed root", () => {
  const root = mkdtempSync(path.join("/private/tmp", "rightkit-storage-doctor-"));
  try {
    const managed = path.join(root, "managed");
    const repoCache = path.join(root, "cache");
    const lexicalManaged = path.join(root, "managed-cache");
    const managedCache = path.join(managed, "artifacts");
    mkdirSync(repoCache, { recursive: true });
    mkdirSync(lexicalManaged, { recursive: true });
    mkdirSync(managedCache, { recursive: true });
    writeFileSync(path.join(repoCache, "large.bin"), "123456");
    writeFileSync(path.join(lexicalManaged, "large.bin"), "123456");
    writeFileSync(path.join(managedCache, "large.bin"), "123456");
    const findings = findLargeRepositorySubdirectories({ appRoot: root, managedRoot: managed, thresholdBytes: 5, ignored: () => true });
    assert.deepEqual(findings.map(({ path: findingPath }) => findingPath).sort(), [lexicalManaged, repoCache].sort());
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("managed root inside repository is skipped only when Git ignores it", () => {
  const root = mkdtempSync(path.join("/private/tmp", "rightkit-storage-doctor-git-"));
  try {
    execFileSync("git", ["init", "-q", root]);
    const managed = path.join(root, "managed");
    mkdirSync(managed, { recursive: true });
    writeFileSync(path.join(managed, "large.bin"), "123456");
    assert.equal(findLargeRepositorySubdirectories({ appRoot: root, managedRoot: managed, thresholdBytes: 5 }).some(({ path: findingPath }) => findingPath === managed), true);
    writeFileSync(path.join(root, ".gitignore"), "managed/\n");
    assert.equal(findLargeRepositorySubdirectories({ appRoot: root, managedRoot: managed, thresholdBytes: 5 }).some(({ path: findingPath }) => findingPath === managed), false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("storage threshold uses decimal five gigabytes", () => {
  assert.equal(DEFAULT_STORAGE_THRESHOLD_BYTES, 5_000_000_000);
});
