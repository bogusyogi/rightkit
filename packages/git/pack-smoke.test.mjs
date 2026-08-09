// BLOCK-recurrence guard: the only test that catches a `files`-allowlist
// omission in package.json is one that actually packs the tarball npm would
// publish and runs the extracted bin with `node` — `node --test` run from
// the source tree never notices a missing directory because the source
// tree has every file regardless of what `files` declares. This test packs
// for real (npm pack), extracts to a temp dir, and executes the built bin's
// --help and --version from inside that extracted tree only.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));

function run(command, args, options) {
  const result = spawnSync(command, args, { encoding: "utf8", ...options });
  if (result.error) throw result.error;
  return result;
}

test("packaged tarball: right-git --help and --version run clean from the published files only", () => {
  const workDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-git-pack-"));
  try {
    // shell:true — on Windows "npm" only resolves (to npm.cmd) through the
    // shell; args here are fixed literals we construct, never user input.
    const packResult = run("npm", ["pack", "--json", "--pack-destination", workDir], { cwd: packageRoot, shell: true });
    assert.equal(packResult.status, 0, `npm pack failed: ${packResult.stderr}`);
    const [packInfo] = JSON.parse(packResult.stdout);

    // Run tar FROM the temp dir on the bare filename so no absolute "C:\..."
    // path reaches tar — that colon is the only reason GNU tar's --force-local
    // was ever needed, and avoiding it keeps this portable to bsdtar (the
    // Windows built-in) and any CI host, not just a GNU-tar toolchain.
    const extractResult = run("tar", ["-xzf", packInfo.filename], { cwd: workDir });
    assert.equal(extractResult.status, 0, `tar extract failed: ${extractResult.stderr}`);
    // npm tarballs always nest content under a top-level "package/" directory.
    const extractedPackageDir = path.join(workDir, "package");
    const bin = path.join(extractedPackageDir, "cli", "right-git.mjs");

    const helpResult = run("node", [bin, "--help"], { cwd: extractedPackageDir });
    assert.equal(helpResult.status, 0, `--help exited non-zero: ${helpResult.stderr}`);
    assert.ok(!/ERR_MODULE_NOT_FOUND/.test(helpResult.stderr ?? ""), helpResult.stderr);
    assert.ok(/right-git/i.test(helpResult.stdout));

    const versionResult = run("node", [bin, "--version"], { cwd: extractedPackageDir });
    assert.equal(versionResult.status, 0, `--version exited non-zero: ${versionResult.stderr}`);
    assert.ok(!/ERR_MODULE_NOT_FOUND/.test(versionResult.stderr ?? ""), versionResult.stderr);
    assert.match(versionResult.stdout, /\d+\.\d+\.\d+/);
  } finally {
    rmSync(workDir, { recursive: true, force: true });
  }
});
