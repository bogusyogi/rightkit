import assert from "node:assert/strict";
import test from "node:test";
import {
  isTerminalStatus,
  laneFromGgufReceipt,
  parseOptions,
  transportInvocation,
} from "./sync_checkpoints_windows.mjs";

test("Windows watcher binds one remote run, bundler, instance, and private destination", () => {
  const parsed = parseOptions([
    "--remote",
    "root@vast-host",
    "--remote-run-dir",
    "/workspace/heardright-full/run",
    "--destination-root",
    "C:\\private\\checkpoints",
    "--poll-seconds",
    "5",
    "--instance-id",
    "45523748",
    "--remote-bundler",
    "/workspace/finalize_remote_run_bundle.py",
  ]);
  assert.equal(parsed.remote, "root@vast-host");
  assert.equal(parsed.pollSeconds, 5);
  assert.equal(parsed.once, false);
  assert.equal(parsed.instanceId, "45523748");
});

test("Windows watcher rejects shell-shaped remote values", () => {
  assert.throws(
    () => parseOptions([
      "--remote",
      "root@host;rm",
      "--remote-run-dir",
      "/workspace/run",
      "--destination-root",
      "C:\\private",
    ]),
    /unsupported/u,
  );
});

test("watcher transports cannot inherit stdin or prompt interactively", () => {
  const invocation = transportInvocation("scp", ["remote:file", "local"]);
  assert.deepEqual(invocation.stdio, ["ignore", "inherit", "inherit"]);
  assert.equal(invocation.args.includes("BatchMode=yes"), true);
  assert.equal(invocation.args.includes("ConnectTimeout=15"), true);
});

test("terminal statuses and lane artifact receipt names are explicit", () => {
  assert.equal(isTerminalStatus("completed_no_viable_candidates"), true);
  assert.equal(isTerminalStatus("running"), false);
  assert.equal(laneFromGgufReceipt("/run/stage-receipts/granite--gguf-evaluate.json"), "granite");
  assert.equal(laneFromGgufReceipt("/run/stage-receipts/granite--train.json"), null);
});
