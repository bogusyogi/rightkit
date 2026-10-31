// E2E L3-05 (audit GB-16, the sidecar half): a sidecar build inside the door's source split resolves Cargo's output directory from the SNAPSHOT's committed
// manifests and maps it to the checkout, never by running `cargo metadata` in the checkout. The checkout may hold anything: a newer HEAD that deleted the
// crate's directory (the selected older commit still has it), a dirty manifest, a different build.target-dir.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { stageSidecars } from "./stage-sidecar.mjs";

const TRIPLE = "x86_64-unknown-linux-gnu";
const stream = (bin, executable) => `${JSON.stringify({ reason: "compiler-artifact", executable, target: { name: bin, kind: ["bin"] } })}\n`;

/** snapshot/app holds the committed manifest; checkout/app is what a newer HEAD left: no Cargo.toml at all (the directory was deleted). */
function split(t, { checkoutManifest = null } = {}) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-pin-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const snapshot = path.join(dir, "snapshot", "app"); const checkout = path.join(dir, "checkout", "app");
  fs.mkdirSync(snapshot, { recursive: true }); fs.mkdirSync(checkout, { recursive: true });
  fs.writeFileSync(path.join(snapshot, "Cargo.toml"), '[package]\nname = "eng"\nversion = "0.1.0"\n');
  if (checkoutManifest !== null) fs.writeFileSync(path.join(checkout, "Cargo.toml"), checkoutManifest);
  return { dir, snapshot, checkout };
}

/** A `rustright cargo` stand-in that answers `metadata` like cargo does: from the Cargo.toml in the directory it is asked about, `target` beside it. */
function fakeCargo({ snapshot, checkout }) {
  const calls = [];
  const run = (command, args, options) => {
    calls.push({ command, args, cwd: options.cwd, targetDir: options.env?.CARGO_TARGET_DIR });
    if (args[1] === "metadata") {
      const manifest = args.includes("--manifest-path") ? args[args.indexOf("--manifest-path") + 1] : path.join(options.cwd, "Cargo.toml");
      if (!fs.existsSync(manifest)) return { status: 101, stdout: "", stderr: `error: failed to read \`${manifest}\`\n\nCaused by:\n  No such file or directory (os error 2)` };
      return { status: 0, stdout: JSON.stringify({ workspace_root: path.dirname(manifest), target_directory: path.join(path.dirname(manifest), "target") }), stderr: "" };
    }
    if (args[1] === "build") {
      const out = path.join(options.env.CARGO_TARGET_DIR, TRIPLE, "release", "eng");
      fs.mkdirSync(path.dirname(out), { recursive: true }); fs.writeFileSync(out, "engine-bytes");
      return { status: 0, stdout: stream("eng", out), stderr: "" };
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  return { run, calls };
}

test("[GB-16 sidecar] the output directory is resolved in the snapshot and mapped to the checkout: a checkout that no longer has the crate's directory changes nothing", async (t) => {
  const roots = split(t);
  const { run, calls } = fakeCargo(roots);
  const result = await stageSidecars({ appRoot: roots.snapshot, cacheRoot: roots.checkout, target: TRIPLE, manifest: [{ kind: "cargo-bin", package: "eng", bin: "eng" }], deps: { now: () => "2026-10-31T00:00:00.000Z", pid: 7, log: () => {}, env: {}, run } });
  assert.equal(result.files[0].path.endsWith("eng-" + TRIPLE), true, JSON.stringify(result.files));
  const metadata = calls.filter((c) => c.args[1] === "metadata");
  assert.ok(metadata.length > 0 && metadata.every((c) => c.cwd === roots.snapshot), `metadata ran in the snapshot, never the checkout: ${JSON.stringify(metadata)}`);
  assert.ok(metadata.every((c) => c.args.includes("--offline")), "and read no registry");
  const build = calls.find((c) => c.args[1] === "build");
  assert.equal(build.cwd, roots.snapshot, "the build runs in the snapshot");
  assert.equal(build.targetDir, path.join(roots.checkout, "target"), "with the output directory mapped onto the checkout's, which the snapshot itself never holds");
  assert.equal(fs.existsSync(path.join(roots.snapshot, "target")), false, "nothing is written under the snapshot");
});

test("[GB-16 sidecar] a dirty or different manifest in the checkout is never read (the snapshot's committed manifest decides)", async (t) => {
  const roots = split(t, { checkoutManifest: "THIS IS NOT A MANIFEST\n" });
  const { run, calls } = fakeCargo(roots);
  await stageSidecars({ appRoot: roots.snapshot, cacheRoot: roots.checkout, target: TRIPLE, manifest: [{ kind: "cargo-bin", package: "eng", bin: "eng", manifestPath: "Cargo.toml" }], deps: { now: () => "2026-10-31T00:00:00.000Z", pid: 7, log: () => {}, env: {}, run } });
  const metadata = calls.filter((c) => c.args[1] === "metadata");
  assert.ok(metadata.length > 0 && metadata.every((c) => c.args[c.args.indexOf("--manifest-path") + 1] === path.join(roots.snapshot, "Cargo.toml")), JSON.stringify(metadata));
  assert.equal(calls.find((c) => c.args[1] === "build").targetDir, path.join(roots.checkout, "target"));
});
