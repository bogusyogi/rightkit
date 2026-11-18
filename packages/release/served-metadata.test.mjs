import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { directReadSpawn } from "./snapshot-metadata.mjs";

test("NN10: a snapshot's cargo metadata is taken from the resident gate's answer when it has one, and Cargo is started only when it has not", async () => {
  const snap = fs.mkdtempSync(path.join(os.tmpdir(), "rk-served-")); const tmpRoot = path.join(snap, "..", `${path.basename(snap)}-tmp`);
  const started = []; const asked = [];
  const spawn = (cmd, args) => { started.push([cmd, ...args].join(" ")); return { status: 0, signal: null, error: null, stdout: "{\"from\":\"cargo\"}", stderr: "" }; };
  let holds = true;
  const served = ({ cwd, args }) => { asked.push(args.join(" ")); return holds ? { status: 0, stdout: "{\"from\":\"resident\"}", stderr: "" } : null; };
  const make = (isAsync) => directReadSpawn({ spawn, cargoCommand: ["cargo"], snapshotRoot: snap, directPath: "", tmpRoot, env: {}, memo: new Map(), isAsync, served });
  const args = ["metadata", "--no-deps", "--format-version", "1", "--locked"];
  assert.equal(make(false)("cargo", args, { cwd: snap }).stdout, "{\"from\":\"resident\"}");
  assert.equal((await make(true)("cargo", args, { cwd: snap })).stdout, "{\"from\":\"resident\"}", "the async scan path too");
  assert.deepEqual(started, [], "no Cargo started while the resident gate answers");
  holds = false;
  assert.equal(make(false)("cargo", args, { cwd: snap }).stdout, "{\"from\":\"cargo\"}");
  assert.deepEqual(started, [`cargo ${args.join(" ")}`], "no answer: Cargo runs as before");
  // Only metadata is ever asked of it; a fetch runs here.
  make(false)("cargo", ["fetch", "--locked"], { cwd: snap });
  assert.equal(asked.filter((a) => a.startsWith("fetch")).length, 0);
  // A call outside the snapshot never reaches it.
  const before = asked.length; make(false)("cargo", args, { cwd: os.tmpdir() }); assert.equal(asked.length, before);
  fs.rmSync(snap, { recursive: true, force: true }); fs.rmSync(tmpRoot, { recursive: true, force: true });
});
