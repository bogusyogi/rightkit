import assert from "node:assert/strict";
import test from "node:test";
import {
  buildCargoPublishPlan,
  packageFileViolations,
  parseCargoPublishArgs,
} from "./publish-cargo.mjs";

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
    "package location",
    "registry publish",
  ]);
  assert.equal(plan.at(-1)?.internal, "upload-archive");
  assert.ok(!plan.some((step) => step.args?.[0] === "publish" && !step.args.includes("--dry-run")), "real Cargo publish must never repackage source");
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
