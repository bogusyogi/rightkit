// BLOCK-recurrence guard: the only test that catches a `files`-allowlist
// omission in package.json is one that actually packs the tarball npm would
// publish and runs the extracted bin with `node` — `node --test` run from
// the source tree never notices a missing directory because the source
// tree has every file regardless of what `files` declares. This test packs
// for real (npm pack), extracts to a temp dir, and executes the built bin's
// --help and --version from inside that extracted tree only.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import packageJson from "./package.json" with { type: "json" };

const packageRoot = path.dirname(fileURLToPath(import.meta.url));

function run(command, args, options) {
  const result = spawnSync(command, args, { encoding: "utf8", ...options });
  if (result.error) throw result.error;
  return result;
}

test("packaged tarball: public exports plus right-ax --help and --version run from published files only", () => {
  const workDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-ax-pack-"));
  try {
    // shell:true — on Windows "npm" only resolves (to npm.cmd) through the
    // shell; args here are fixed literals we construct, never user input.
    const packResult = run("npm", ["pack", "--json", "--pack-destination", workDir], { cwd: packageRoot, shell: true });
    assert.equal(packResult.status, 0, `npm pack failed: ${packResult.stderr}`);
    const [packInfo] = JSON.parse(packResult.stdout);
    const packedPaths = packInfo.files.map((entry) => entry.path.replaceAll("\\", "/"));
    assert.ok(!packedPaths.some((file) => file.endsWith(".test.mjs")), "published tarball must exclude tests");
    assert.ok(!packedPaths.some((file) => file.includes("/fixtures/") || file.startsWith("fixtures/")), "published tarball must exclude fixtures");
    assert.ok(!packedPaths.some((file) => file.includes("/preview/") || file.startsWith("preview/")), "published tarball must exclude previews");
    for (const required of ["cli/right-ax.mjs", "adopt.mjs", "manifest.mjs", "manifest.schema.json", "ax/adapter.mjs", "ax/adversarial.mjs", "ax/artifact.mjs", "lib/version.mjs"]) assert.ok(packedPaths.includes(required), `missing packed runtime file: ${required}`);

    // Run tar FROM the temp dir on the bare filename so no absolute "C:\..."
    // path reaches tar — that colon is the only reason GNU tar's --force-local
    // was ever needed, and avoiding it keeps this portable to bsdtar (the
    // Windows built-in) and any CI host, not just a GNU-tar toolchain.
    const extractResult = run("tar", ["-xzf", packInfo.filename], { cwd: workDir });
    assert.equal(extractResult.status, 0, `tar extract failed: ${extractResult.stderr}`);
    // npm tarballs always nest content under a top-level "package/" directory.
    const extractedPackageDir = path.join(workDir, "package");
    const bin = path.join(extractedPackageDir, "cli", "right-ax.mjs");
    const importProbe = path.join(extractedPackageDir, "pack-imports.mjs");
    writeFileSync(importProbe, `await Promise.all([\n  import("@rightkit/ax/manifest"),\n  import("@rightkit/ax/ax/static"),\n  import("@rightkit/ax/ax/conformance"),\n  import("@rightkit/ax/ax/behavioral"),\n  import("@rightkit/ax/ax/adversarial"),\n  import("@rightkit/ax/ax/report"),\n  import("@rightkit/ax/plugin/scaffold"),\n  import("@rightkit/ax/plugin/validate"),\n]);\n`);
    const importResult = run("node", [importProbe], { cwd: extractedPackageDir });
    assert.equal(importResult.status, 0, `public export import failed: ${importResult.stderr}`);

    const helpResult = run("node", [bin, "--help"], { cwd: extractedPackageDir });
    assert.equal(helpResult.status, 0, `--help exited non-zero: ${helpResult.stderr}`);
    assert.ok(!/ERR_MODULE_NOT_FOUND/.test(helpResult.stderr ?? ""), helpResult.stderr);
    assert.ok(/right-ax <command>/.test(helpResult.stdout));

    const versionResult = run("node", [bin, "--version"], { cwd: extractedPackageDir });
    assert.equal(versionResult.status, 0, `--version exited non-zero: ${versionResult.stderr}`);
    assert.ok(!/ERR_MODULE_NOT_FOUND/.test(versionResult.stderr ?? ""), versionResult.stderr);
    assert.equal(versionResult.stdout.trim(), `right-ax ${packageJson.version}`);
  } finally {
    rmSync(workDir, { recursive: true, force: true });
  }
});
