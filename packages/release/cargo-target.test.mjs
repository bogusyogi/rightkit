import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { resolveTargetRoot } from "./cargo-target.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(here, "cargo-target.mjs");

// A fake `cargo` on PATH, ahead of the real one, so metadata success/failure
// shapes are deterministic and don't depend on the host's actual crate graph.
// It branches on a marker embedded in the manifest path so each test controls
// its own outcome independently.
function fakeCargoBin() {
  const bin = mkdtempSync(path.join(os.tmpdir(), "rightkit-fake-cargo-"));
  const script = path.join(bin, process.platform === "win32" ? "cargo.cmd" : "cargo");
  const body = process.platform === "win32"
    ? [
      "@echo off",
      "setlocal enabledelayedexpansion",
      "set ARGS=%*",
      "echo %ARGS% | findstr /C:\"fail-manifest\" >nul && (echo boom 1>&2 & exit /b 1)",
      "echo %ARGS% | findstr /C:\"bad-target-manifest\" >nul && (echo {\"target_directory\":\"relative/target\"} & exit /b 0)",
      "echo {\"target_directory\":\"" + path.join(bin, "target").replaceAll("\\", "\\\\") + "\"}",
    ].join("\r\n")
    : [
      "#!/bin/sh",
      'case "$*" in',
      '  *fail-manifest*) echo boom 1>&2; exit 1 ;;',
      '  *bad-target-manifest*) echo \'{"target_directory":"relative/target"}\'; exit 0 ;;',
      `  *) echo '{"target_directory":"${path.join(bin, "target")}"}'; exit 0 ;;`,
      "esac",
    ].join("\n");
  writeFileSync(script, body);
  if (process.platform !== "win32") chmodSync(script, 0o755);
  return bin;
}

function withFakeCargo(fn) {
  const bin = fakeCargoBin();
  const env = { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH ?? ""}` };
  return fn({ bin, env });
}

test("resolveTargetRoot returns the absolute target_directory Cargo metadata reports", () => {
  withFakeCargo(({ bin, env }) => {
    const manifestDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-"));
    const manifest = path.join(manifestDir, "Cargo.toml");
    writeFileSync(manifest, "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n");
    const previousPath = process.env.PATH;
    process.env.PATH = env.PATH;
    try {
      const result = resolveTargetRoot(manifest);
      assert.equal(result, path.resolve(path.join(bin, "target")));
    } finally {
      process.env.PATH = previousPath;
    }
  });
});

test("resolveTargetRoot fails closed, naming the manifest, when target_directory is not absolute", () => {
  withFakeCargo(({ env }) => {
    const manifestDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-"));
    const manifest = path.join(manifestDir, "bad-target-manifest-Cargo.toml");
    writeFileSync(manifest, "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n");
    const previousPath = process.env.PATH;
    process.env.PATH = env.PATH;
    try {
      assert.throws(
        () => resolveTargetRoot(manifest),
        new RegExp(`metadata for .*bad-target-manifest-Cargo\\.toml did not report an absolute target_directory`),
      );
    } finally {
      process.env.PATH = previousPath;
    }
  });
});

test("resolveTargetRoot fails closed, naming the manifest, when the metadata command exits non-zero", () => {
  withFakeCargo(({ env }) => {
    const manifestDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-"));
    const manifest = path.join(manifestDir, "fail-manifest-Cargo.toml");
    writeFileSync(manifest, "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n");
    const previousPath = process.env.PATH;
    process.env.PATH = env.PATH;
    try {
      assert.throws(
        () => resolveTargetRoot(manifest),
        new RegExp(`metadata failed for .*fail-manifest-Cargo\\.toml`),
      );
    } finally {
      process.env.PATH = previousPath;
    }
  });
});

test("resolveTargetRoot throws when no manifest path is given", () => {
  assert.throws(() => resolveTargetRoot(), /resolveTargetRoot requires a Cargo.toml manifest path/);
  assert.throws(() => resolveTargetRoot(""), /resolveTargetRoot requires a Cargo.toml manifest path/);
});

test("CLI mode writes only the resolved path to stdout on success", () => {
  withFakeCargo(({ bin, env }) => {
    const manifestDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-"));
    const manifest = path.join(manifestDir, "Cargo.toml");
    writeFileSync(manifest, "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n");
    const result = spawnSync(process.execPath, [cli, manifest], { encoding: "utf8", env });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), path.resolve(path.join(bin, "target")));
  });
});

test("CLI mode writes a message to stderr and exits 1 on failure", () => {
  withFakeCargo(({ env }) => {
    const manifestDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-"));
    const manifest = path.join(manifestDir, "fail-manifest-Cargo.toml");
    writeFileSync(manifest, "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n");
    const result = spawnSync(process.execPath, [cli, manifest], { encoding: "utf8", env });
    assert.equal(result.status, 1);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /metadata failed for .*fail-manifest-Cargo\.toml/);
  });
});

test("CLI mode requires a manifest argument", () => {
  const result = spawnSync(process.execPath, [cli], { encoding: "utf8" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /usage: cargo-target\.mjs/);
});
