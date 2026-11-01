// rightkit#1367 step 2: the broker, not the gate, decides how many builds run. N components submitted at once are QUEUED or run by the real broker; none is refused for capacity.
// This runs the installed RustRight client against an isolated instance of the installed broker (test-support/real-broker.mjs), like the other real-broker tests: it is skipped
// only where they are (Windows, POSIX wrapper), and a host with no installed broker fails it loudly. STATUS: not yet observed. No run of this file against a real broker is on record;
// until one is, its assertions are the intended proof, not an observed one.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { brokerStateOf } from "./broker-admission.mjs";
import { realChildProcess } from "./test-support/broker-guard.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";
import { readRequestRef } from "./snapshot-request.mjs";

const POSIX = process.platform === "win32" ? "the runtime wrapper is POSIX only: a Windows child finds only an .exe on PATH; the Dell refuses by the static walk" : false;
const COMPONENTS = 6;
/** What a capacity refusal looks like on the client's output: busy pipe, paused or draining admission, an exclusive owner. A queued job prints none of these. */
const CAPACITY_REFUSAL = /RK_BROKER_BUSY|RK_ADMISSION_|RK_CUTOVER_DRAIN|already (?:owns|owned|running)|another (?:build|gate)|exclusive/i;

const runClient = (client, args, options) => new Promise((resolve) => {
  const child = realChildProcess.spawn(client, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = ""; let stderr = "";
  child.stdout.on("data", (c) => { stdout += c; }); child.stderr.on("data", (c) => { stderr += c; });
  child.once("error", (error) => resolve({ status: null, stdout, stderr, error }));
  child.once("close", (status) => resolve({ status, stdout, stderr }));
});

test("real broker: N components submitted at once are queued or run, never refused for capacity", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "burst" });
  t.after(() => rb.stop());
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-burst-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 }));
  const jobs = [];
  for (let i = 0; i < COMPONENTS; i += 1) {
    const dir = path.join(root, `c${i}`);
    fs.mkdirSync(path.join(dir, "src"), { recursive: true });
    fs.writeFileSync(path.join(dir, "Cargo.toml"), `[package]\nname = "burst${i}"\nversion = "0.1.0"\nedition = "2021"\n`);
    fs.writeFileSync(path.join(dir, "src", "lib.rs"), "pub fn f() {}\n");
    jobs.push({ dir, ref: path.join(root, `ref${i}.json`) });
  }
  // Every client starts before any has finished: the broker sees N components of one repository at once.
  const clients = jobs.map((job) => runClient(rb.clientPath, ["cargo", "check"], { cwd: job.dir, env: { ...rb.env, RUSTRIGHT_REQUEST_ID_FILE: job.ref } }));
  const results = await Promise.all(clients);
  const ids = jobs.map((job) => { try { return readRequestRef(job.ref).requestId; } catch { return null; } });
  for (const [i, result] of results.entries()) {
    assert.doesNotMatch(`${result.stdout}\n${result.stderr}`, CAPACITY_REFUSAL, `component ${i}: a build the broker queues is never refused for capacity`);
    assert.ok(ids[i], `component ${i}: the client wrote its request reference at submit (it was admitted to the broker's queue)`);
    const status = await runClient(rb.clientPath, ["status", ids[i]], { env: rb.env });
    const state = brokerStateOf(status.stdout, ids[i]);
    assert.ok(["SUCCEEDED", "FAILED"].includes(state), `component ${i}: the request ended (${state}), it was not left queued or rejected`);
  }
  assert.equal(new Set(ids).size, COMPONENTS, "N distinct requests");
});
