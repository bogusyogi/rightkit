import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { LINUX_ARTIFACTS_SCHEMA, glibcFloor, linuxArch, packLinuxArtifacts, validateLinuxArchive } from "./linux-artifacts.mjs";

const COMMIT = "a".repeat(40);
const fakeRun = (arch, objdump = "") => (cmd, args, opts) => {
  if (cmd === "uname") return { status: 0, stdout: `${arch}\n` };
  if (cmd === "objdump") return { status: 0, stdout: objdump };
  return spawnSync(cmd, args, { ...opts, stdio: "pipe" });
};
const ELF = Buffer.concat([Buffer.from([0x7f, 0x45, 0x4c, 0x46]), Buffer.alloc(60)]);

function app(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-linux-art-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, "dist", "bin"), { recursive: true });
  fs.writeFileSync(path.join(root, "dist", "bin", "pulse"), ELF);
  fs.writeFileSync(path.join(root, "dist", "README"), "hello\n");
  return root;
}

test("archive config is strict: name stem, non-empty relative includes, no unknown keys", () => {
  assert.equal(validateLinuxArchive(undefined), null);
  assert.deepEqual(validateLinuxArchive({ name: "pulse", include: ["dist"] }), { name: "pulse", include: ["dist"] });
  for (const bad of [{ name: "Pulse", include: ["dist"] }, { name: "pulse", include: [] }, { name: "pulse", include: ["/abs"] }, { name: "pulse", include: ["../up"] }, { name: "pulse", include: ["dist"], url: "x" }]) {
    assert.throws(() => validateLinuxArchive(bad), /archive/);
  }
});

test("architecture is uname -m and only x86_64 or aarch64", () => {
  assert.equal(linuxArch(fakeRun("x86_64")), "x86_64");
  assert.equal(linuxArch(fakeRun("aarch64")), "aarch64");
  assert.throws(() => linuxArch(fakeRun("arm64")), /unsupported/);
});

test("glibc floor is the highest GLIBC_ version any ELF file needs; non-ELF files are skipped; an objdump failure fails closed", (t) => {
  const root = app(t);
  const files = [path.join(root, "dist", "bin", "pulse"), path.join(root, "dist", "README")];
  assert.equal(glibcFloor(files, { run: fakeRun("x86_64", "x GLIBC_2.17 y GLIBC_2.34 z GLIBC_2.4\n") }), "2.34");
  assert.throws(() => glibcFloor(files, { run: () => ({ status: 1, stdout: "" }) }), /glibc floor cannot be stated/);
});

test("pack writes one tarball per arch with its sha256 and size, merges arches of the same build, and replaces a manifest of another build", (t) => {
  const root = app(t);
  const archive = { name: "pulse", include: ["dist"] };
  const x = packLinuxArtifacts({ root, archive, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64", "GLIBC_2.35"), log: () => {} });
  const tarPath = path.join(root, ".right-release", "linux", "pulse-0.1.0-linux-x86_64.tar.gz");
  const bytes = fs.readFileSync(tarPath);
  assert.equal(x.sha256, createHash("sha256").update(bytes).digest("hex"));
  assert.equal(x.size, bytes.length);
  assert.ok(/^dist\/bin\/pulse$/m.test(spawnSync("tar", ["-tzf", tarPath], { encoding: "utf8" }).stdout), "a listed folder keeps its own tree under its base name");
  packLinuxArtifacts({ root, archive, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("aarch64", "GLIBC_2.39"), log: () => {} });
  let m = JSON.parse(fs.readFileSync(x.manifestPath, "utf8"));
  assert.equal(m.schema, LINUX_ARTIFACTS_SCHEMA);
  assert.deepEqual(Object.keys(m.artifacts).sort(), ["aarch64", "x86_64"]);
  assert.deepEqual(m.artifacts.x86_64, { file: "pulse-0.1.0-linux-x86_64.tar.gz", sha256: x.sha256, size: x.size, glibc: "2.35" });
  assert.equal(m.artifacts.aarch64.glibc, "2.39");
  packLinuxArtifacts({ root, archive, app: "pulse", version: "0.1.1", commit: COMMIT, run: fakeRun("x86_64", "GLIBC_2.35"), log: () => {} });
  m = JSON.parse(fs.readFileSync(x.manifestPath, "utf8"));
  assert.deepEqual(Object.keys(m.artifacts), ["x86_64"], "another version starts a new manifest, never mixed");
  assert.throws(() => packLinuxArtifacts({ root, archive, app: "pulse", version: "x", commit: COMMIT, run: fakeRun("x86_64"), log: () => {} }), /semver/);
  assert.throws(() => packLinuxArtifacts({ root, archive: { name: "pulse", include: ["missing"] }, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64"), log: () => {} }), /does not exist/);
});

test("the archive is flat: each listed file is stored under its base name; duplicate base names are refused", (t) => {
  const root = app(t);
  fs.mkdirSync(path.join(root, "scripts", "linux-guest"), { recursive: true });
  fs.writeFileSync(path.join(root, "scripts", "linux-guest", "install.sh"), "#!/bin/sh\n");
  const r = packLinuxArtifacts({ root, archive: { name: "pulse", include: ["dist/bin/pulse", "scripts/linux-guest/install.sh"] }, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64", "GLIBC_2.35"), log: () => {} });
  const entries = spawnSync("tar", ["-tzf", path.join(path.dirname(r.manifestPath), r.file)], { encoding: "utf8" }).stdout.trim().split("\n").sort();
  assert.deepEqual(entries, ["install.sh", "pulse"]);
  fs.mkdirSync(path.join(root, "other"), { recursive: true }); fs.writeFileSync(path.join(root, "other", "pulse"), "x");
  assert.throws(() => packLinuxArtifacts({ root, archive: { name: "pulse", include: ["dist/bin/pulse", "other/pulse"] }, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64"), log: () => {} }), /share the base name pulse/);
});
