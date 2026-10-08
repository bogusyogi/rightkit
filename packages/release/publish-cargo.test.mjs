import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync, rmSync, mkdirSync, readFileSync, readdirSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildCargoPublishPlan,
  packageFileViolations,
  parseCargoPublishArgs,
  runCargoPublish,
} from "./publish-cargo.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { checkPublicationGate } from "./publication-gate.mjs";

test("cargo publish requires a safe explicit crate name", () => {
  assert.throws(() => parseCargoPublishArgs([]), /--crate/);
  assert.throws(() => parseCargoPublishArgs(["--crate", "../secret"]), /invalid crate/);
  assert.throws(() => parseCargoPublishArgs(["--crate", "rightkit-license", "--allow-dirty"]), /only valid with --dry-run/);
  assert.equal(parseCargoPublishArgs(["--crate", "rightkit-license"]).crate, "rightkit-license");
});

test("cargo publish plan always runs local safety gates before upload", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "C:/rightkit/crates/rightkit-license", dryRun: false });
  assert.deepEqual(plan.map((step) => step.label), [
    "secret scan",
    "format",
    "tests",
    "clippy",
    "package contents",
    "registry dry-run",
    "package archive",
    "package location",
    "archive hash check",
    "registry publish",
    "registry readback",
  ]);
  assert.deepEqual(plan.find((step) => step.label === "package archive").args, ["package", "--no-verify", "-p", "rightkit-license"]);
  assert.equal(plan.find((step) => step.label === "archive hash check").internal, "verify-archive");
  assert.deepEqual(plan.find((step) => step.label === "registry publish"), {
    label: "registry publish", command: "cargo", args: ["publish", "--no-verify", "-p", "rightkit-license"],
  });
  assert.equal(plan.at(-1)?.internal, "registry-readback");
  assert.deepEqual(plan.find((step) => step.label === "format").args, ["fmt", "-p", "rightkit-license", "--check"]);
  assert.deepEqual(plan.find((step) => step.label === "tests").args, ["test", "-p", "rightkit-license", "--", "--test-threads=1"]);
});

test("dry-run plan never contains a real registry upload", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-logs", crateDir: "C:/rightkit/crates/rightkit-logs", dryRun: true });
  assert.equal(plan.at(-1)?.label, "registry dry-run");
});

test("package inspection rejects secret and build-output paths", () => {
  assert.deepEqual(packageFileViolations([
    "Cargo.toml",
    "src/lib.rs",
    ".env",
    "keys/signing.p8",
    "target/release/app",
  ]), [".env", "keys/signing.p8", "target/release/app"]);
});

function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-publish-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const name = "rightkit-fixture", version = "1.0.0", bytes = Buffer.from("fixture crate archive");
  const crateDir = path.join(root, "crates", name), target = path.join(root, "generated-target");
  mkdirSync(crateDir, { recursive: true });
  writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "${version}"\n`);
  mkdirSync(path.join(target, "package"), { recursive: true });
  const crateFile = path.join(target, "package", `${name}-${version}.crate`);
  writeFileSync(crateFile, bytes);
  const evidence = { artifactSha256: digestBytes(bytes) }, events = [];
  const execute = (command, args, settings) => {
    assert.equal(command, "cargo");
    assert.equal(settings.cwd, root);
    assert.equal(settings.windowsHide, true);
    if (args[0] === "package" && args.includes("--no-verify")) events.push("package");
    if (args[0] === "publish" && !args.includes("--dry-run")) {
      assert.deepEqual(args, ["publish", "--no-verify", "-p", name]);
      events.push("publish");
    }
    return { status: 0, stdout: args[0] === "metadata" ? JSON.stringify({ target_directory: target })
      : args.includes("--list") ? "Cargo.toml\nsrc/lib.rs\n" : "" };
  };
  let gateCalls = 0;
  const deps = {
    enforceGate: () => { if (++gateCalls === 2) events.push("hash check"); return { ok: true, evidence }; },
    fetch: async () => {
      events.push("readback");
      return { ok: true, text: async () => JSON.stringify({ name, vers: version, cksum: evidence.artifactSha256 }) };
    },
  };
  return { root, name, version, bytes, crateFile, evidence, events, execute, deps, options: { workspaceRoot: root, crate: name } };
}

test("Cargo publisher packages, checks evidence hash, publishes through same runner & reads back", async (t) => {
  const fx = fixture(t);
  const result = await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.deepEqual(fx.events, ["package", "hash check", "publish", "readback"]);
  assert.deepEqual(result, { name: fx.name, version: fx.version, sha256: fx.evidence.artifactSha256 });
});

test("archive hash mismatch refuses publication before Cargo upload or readback", async (t) => {
  const fx = fixture(t);
  writeFileSync(fx.crateFile, "different archive");
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: "PUBLICATION_HASH_MISMATCH" });
  assert.deepEqual(fx.events, ["package", "hash check"]);
});

test("changed publication evidence refuses Cargo upload", async (t) => {
  const fx = fixture(t);
  const execute = (command, args, settings) => {
    const result = fx.execute(command, args, settings);
    if (args[0] === "package" && args.includes("--no-verify")) fx.evidence.artifactSha256 = "b".repeat(64);
    return result;
  };
  await assert.rejects(runCargoPublish(fx.options, execute, fx.deps), /evidence changed during packaging/);
  assert.deepEqual(fx.events, ["package", "hash check"]);
});

test("failed Cargo publish prevents registry readback", async (t) => {
  const fx = fixture(t);
  const execute = (command, args, settings) => {
    const result = fx.execute(command, args, settings);
    return args[0] === "publish" && !args.includes("--dry-run") ? { status: 1 } : result;
  };
  await assert.rejects(runCargoPublish(fx.options, execute, fx.deps), /registry publish failed/);
  assert.deepEqual(fx.events, ["package", "hash check", "publish"]);
});

test("registry mismatch propagates from publisher without executing yank", async (t) => {
  const fx = fixture(t);
  fx.deps.fetch = async () => ({ ok: true, text: async () => JSON.stringify({ name: fx.name, vers: fx.version, cksum: "b".repeat(64) }) });
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: "PUBLICATION_REGISTRY_MISMATCH" });
  assert.deepEqual(fx.events, ["package", "hash check", "publish"]);
});

test("dry-run publisher never checks an archive, uploads or reads registry", async (t) => {
  const fx = fixture(t);
  assert.deepEqual(await runCargoPublish({ ...fx.options, dryRun: true }, fx.execute, fx.deps),
    { name: fx.name, version: fx.version, dryRun: true });
  assert.deepEqual(fx.events, []);
});

test("publication gate compares actual artifact buffer to evidence", () => {
  const commit = "a".repeat(40), bytes = Buffer.from("fixture crate archive"), name = "rightkit-fixture", version = "1.0.0";
  const options = { kind: "cargo", name, version, packagePath: "/fixture", evidencePath: "/evidence", artifactBytes: bytes,
    git: (args) => args[0] === "rev-parse" ? commit : "",
    readFile: () => JSON.stringify({ schema: 1, kind: "cargo", name, version, commit, artifactSha256: digestBytes(bytes),
      runs: ["darwin", "win32"].map((os) => ({ host: { os }, commit, command: "node --test", passed: 1, failed: 0, operator: "test", timestamp: "2026-10-08" })) }),
  };
  assert.equal(checkPublicationGate(options).ok, true);
  assert.equal(checkPublicationGate({ ...options, artifactBytes: Buffer.from("changed") }).ok, false);
});

test("release source never accesses Cargo credential files or registry token environment variables", () => {
  const root = fileURLToPath(new URL(".", import.meta.url));
  const forbidden = /\bCARGO_REGISTRY_TOKEN\b|\bCARGO_REGISTRIES_[A-Z0-9_]+_TOKEN\b|["'`]credentials(?:\.toml)?["'`]|\.cargo[/\\]credentials(?:\.toml)?/;
  const inspect = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory() && !["node_modules", "fixtures", "test"].includes(entry.name)) inspect(file);
      else if (entry.isFile() && /\.(?:mjs|js|ts|py|sh|ps1)$/.test(entry.name) && !/\.test\./.test(entry.name)) {
        assert.doesNotMatch(readFileSync(file, "utf8"), forbidden, path.relative(root, file));
      }
    }
  };
  inspect(root);
});
