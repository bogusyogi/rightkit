#!/usr/bin/env node
// `node qa-snapshot-run.mjs <rightkit-qa> -- <qa args>`: one `rightkit-qa run-tests` step of a snapshot stage that would compile, as the two
// parts the gate can carry (qa-snapshot.mjs). build-launch.mjs hands such a step to this entry instead of refusing it.
//   (a) the build: the step's test build (`--no-run --message-format=json`) through the carried seam (carried-cargo.mjs runCarriedCargo: one
//       signed, ledgered broker request from this directory), its stdout written to a listing file under the carrier's managed directory,
//       which is outside the snapshot and the primary checkout;
//   (b) the QA step itself, run directly (no broker job: it needs the GUI session and the unscrubbed environment) as the same argv with
//       `--from-cargo-json <listing>` in place of the build arguments.
// The step's own environment is this process's environment (the gate's step spawn merged it), so the direct run gets it as declared.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { carriedCargoMapOf, runCarriedCargo } from "./carried-cargo.mjs";
import { isInside, qaSnapshotPlan, realish } from "./qa-snapshot.mjs";

const fail = (message, status = 101) => { process.stderr.write(`rightkit-gate: ${message}\n`); process.exit(status); };

const [qa, separator, ...args] = process.argv.slice(2);
if (!qa || separator !== "--") fail("RK_QA_SNAPSHOT_USAGE: usage: qa-snapshot-run.mjs <rightkit-qa> -- <args>");
const carried = carriedCargoMapOf(process.env);
if (!carried?.map) fail(`RK_QA_SNAPSHOT_CARRIER_MISSING: ${carried?.error ?? "no carried map in the environment"}; a snapshot stage's build is never run around its carrier`);
const plan = qaSnapshotPlan(args);
if (!plan) fail(`RK_QA_SNAPSHOT_NO_BUILD: rightkit-qa ${args.join(" ")} compiles nothing and is run directly, not through this entry`);
if (plan.refusal) fail(`RK_QA_SNAPSHOT_REFUSED: ${plan.refusal}`);

const { map } = carried;
const section = map.broker ?? {};
const snapshot = section.snapshot?.path;
const checkouts = [section.checkout, process.env.RK_GATE_CHECKOUT].filter((c) => typeof c === "string" && c);
const dir = path.join(path.dirname(map.ledgerDir), "qa-listings");
if ((snapshot && isInside(dir, snapshot)) || checkouts.some((c) => isInside(dir, c))) fail(`RK_QA_SNAPSHOT_LISTING_PLACE: the listing directory ${dir} is inside the snapshot or the primary checkout`);
fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
const listing = path.join(realish(dir), `${process.pid}-${Date.now().toString(36)}-${crypto.randomBytes(4).toString("hex")}.jsonl`);

const built = runCarriedCargo(plan.cargoArgs, { cwd: process.cwd(), env: process.env, stdio: "capture" }, { map });
if (built.stderr) process.stderr.write(built.stderr);
if (built.error) fail(`RK_QA_SNAPSHOT_BUILD: ${built.error.message}`);
if (built.status !== 0 || built.refusal || built.outputDir === undefined) {
  process.stderr.write(`rightkit-gate: RK_QA_SNAPSHOT_BUILD: the carried build \`${plan.cargoArgs.join(" ")}\` did not complete with a verified receipt (exit ${built.status}${built.refusal ? `, ${built.refusal.code}` : ""}); the QA step did not run\n`);
  process.exit(built.status || 101);
}
// The listing is the build's output, not a result: it is removed when the direct QA step ends, whatever its exit code.
let status;
try {
  fs.writeFileSync(listing, built.stdout, { mode: 0o600 });
  const ran = spawnSync(qa, plan.qaArgs(listing), { cwd: process.cwd(), env: process.env, stdio: "inherit", windowsHide: true });
  if (ran.error) process.stderr.write(`rightkit-gate: RK_QA_SNAPSHOT_RUN: ${ran.error.message}\n`);
  status = ran.error ? 101 : ran.status ?? 1;
} finally {
  try { fs.rmSync(listing, { force: true }); } catch { /* best effort: the managed directory is pruned with its run */ }
}
process.exit(status);
