import { strict as assert } from "node:assert";
import { test } from "node:test";
import { classBChainKey, laneOf, publisherRequirementScan } from "./deterministic-scans.mjs";

const requirement = { id: "publisher:cargo:unit:0", coverageKey: "key", workspace: ".", cwd: "crates/unit", executable: "cargo", argv: ["test", "-p", "unit", "--message-format=json-render-diagnostics", "--no-fail-fast", "--", "--test-threads=1", "--show-output"], env: { SECRET_TOKEN: "private" }, phase: "test" };

test("publisher broker tests compile through carrier & run with original libtest arguments", () => {
  const scan = publisherRequirementScan(requirement, { platform: "darwin", nativeArch: "arm64", cargoCommand: ["rustright", "cargo"], cargoLiveRoot: "/checkout", targetDirs: { ".": "/target" }, quiet: true });
  assert.equal(scan.root, "cargoLive");
  assert.deepEqual(scan.runOutside.args, ["--test-threads=1", "--show-output"]);
  assert.ok(scan.args.includes("--message-format=json"));
  assert.ok(!scan.args.includes("--no-fail-fast"));
  assert.equal(scan.env.CARGO_TARGET_DIR, "/target");
  assert.equal(scan.quiet, true);
  assert.equal(scan.command.includes("private"), false);
  assert.equal(classBChainKey(scan, true), ".");
});

test("publisher direct tests keep Cargo execution & workspace identity", () => {
  const scan = publisherRequirementScan(requirement, { platform: "darwin", nativeArch: "arm64" });
  assert.equal(scan.runOutside, undefined);
  assert.ok(scan.args.includes("--no-fail-fast"));
  assert.equal(scan.executionWorkspace, ".");
  assert.equal(laneOf({ ...scan, publisherPhase: "compile" }), "compile");
  assert.throws(() => publisherRequirementScan({ ...requirement, workspace: "../escape" }));
});
