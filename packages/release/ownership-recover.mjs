#!/usr/bin/env node
// Recovery for a build-ownership record left by a gate that died or was stopped (rightkit#1513; Adrian, 2026-11-05: "have proper recovery
// tools for the fucking gate. This recovery avoidance is wrong"; "It should ideally auto recover").
// The gate's own reclaim recovers automatically (build-ownership.mjs coveredWorkGone): a sub-handle the broker answers UNKNOWN with no member
// row was never received when the same broker process has run since before the ownership was acquired and no process of the run remains.
// This tool explains a held record (processes, broker leases, every sub-handle) and, with --apply, runs that reclaim now and releases.
//
//   node ownership-recover.mjs <owner.json> [--apply] [--wait-ms N]
// Dry run by default: prints the evidence and the plan, changes nothing. Exit 0 recovered (or nothing to do / plan clean), 2 refused, 64 usage.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { acquireBuildOwnership, brokerLeaseState, proveOwnerProcessesGone, runningBrokerProcess } from "./build-ownership.mjs";

const SETTLE_MS = 2000;

export async function recover({ file, apply = false, waitMs = 600_000, log = console.log, lease = brokerLeaseState, broker = () => runningBrokerProcess(), sleep = (ms) => new Promise((r) => setTimeout(r, ms)) }) {
  const text = (() => { try { return fs.readFileSync(file, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw e; } })();
  if (text === null) { log(`recover: ${file}: no record; nothing to recover`); return 0; }
  const owner = JSON.parse(text);
  log(`recover: ${file}: held by ${owner.handle} (pid ${owner.pid}, ${owner.stage ?? "stage"} of ${owner.commit ?? "a commit"}) since ${owner.acquiredAt}`);

  // 1. Processes: the owner and everything of its run are gone (the reclaim's own process proof, no broker judged).
  let proof;
  try { proof = proveOwnerProcessesGone({ file }); } catch (e) { log(`recover: REFUSED: ${e.message}`); return 2; }
  for (const p of proof.proofs) log(`  process: ${p}`);

  // 2. Broker: every submission settled. Wait for ACTIVE work of the dead run to finish (a stopped gate leaves its last broker jobs running).
  const subs = [...new Set(Array.isArray(owner.broker?.handles) ? owner.broker.handles : [])];
  const unsent = owner.broker?.unsent ?? {};
  const deadline = Date.now() + waitMs;
  let ownerLease;
  for (;;) {
    ownerLease = lease({ snapshot: owner.snapshot, handle: owner.handle });
    if (ownerLease.state !== "ACTIVE") break;
    if (Date.now() > deadline) { log(`recover: REFUSED: broker work under ${owner.handle} is still ACTIVE after ${Math.round(waitMs / 1000)} s`); return 2; }
    log(`  broker: ${owner.handle} ACTIVE; waiting`); await sleep(5000);
  }
  log(`  broker: owner lease ${ownerLease.state} (${(ownerLease.members ?? []).length} member row(s))`);
  if (!subs.length || !owner.broker?.submitted) log("  broker: no carried request was recorded");

  const brk = broker();
  const acquiredMs = Date.parse(owner.acquiredAt);
  const brokerSawAll = Boolean(brk && Number.isFinite(acquiredMs) && brk.startedMs + SETTLE_MS < acquiredMs);
  log(`  broker process: ${brk ? `pid ${brk.pid} up since ${new Date(brk.startedMs).toISOString()}` : "not established"}; ${brokerSawAll ? "continuously up since before this ownership" : "NOT shown up since before this ownership"}`);

  const toMark = []; const refused = []; let reaped = 0;
  for (const sub of subs) {
    if (unsent[sub]) continue;
    const one = lease({ snapshot: owner.snapshot, handle: sub });
    const listed = (ownerLease.members ?? []).find((m) => m.handle === sub);
    if (one.state === "REAPED") { reaped += 1; continue; }
    if (one.state === "ACTIVE" || (listed && listed.state !== "REAPED")) { refused.push(`${sub}: ${one.state}${listed ? ` (listed ${listed.state})` : ""}`); continue; }
    if (one.state === "UNKNOWN" && Array.isArray(one.members) && one.members.length === 0 && !listed && brokerSawAll) { toMark.push(sub); continue; }
    refused.push(`${sub}: ${one.state} (${one.why ?? ""})${brokerSawAll ? "" : "; the broker was not up throughout, so UNKNOWN proves nothing"}`);
  }
  log(`  sub-handles: ${subs.length} recorded, ${reaped} REAPED, ${Object.keys(unsent).length} already marked unsent, ${toMark.length} never received, ${refused.length} unresolved`);
  if (refused.length) { for (const r of refused.slice(0, 10)) log(`  unresolved: ${r}`); log("recover: REFUSED: some broker work cannot be accounted for"); return 2; }

  // The reclaim itself settles a never-received sub-handle (build-ownership.mjs coveredWorkGone, rightkit#1513): the next gate run recovers this
  // record automatically; --apply runs that same reclaim now and releases.
  if (!apply) { log(`recover: recoverable: ${reaped} REAPED, ${toMark.length} never reached the broker. The next gate run reclaims it automatically; --apply reclaims now.`); return 0; }
  let own;
  try { own = acquireBuildOwnership({ file, stage: "ownership-recover", snapshot: owner.snapshot, commit: owner.commit, exportEnv: false }); }
  catch (e) { log(`recover: REFUSED by the reclaim: ${e.message}`); return 2; }
  for (const p of own.reclaimed?.proofs ?? []) log(`  reclaim: ${p.slice(0, 200)}`);
  const rel = own.release();
  log(`recover: recovered ${owner.handle}; released ${rel?.released === true ? "cleanly" : JSON.stringify(rel).slice(0, 200)}`);
  return 0;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const args = process.argv.slice(2);
  const file = args.find((a) => !a.startsWith("--"));
  const waitAt = args.indexOf("--wait-ms");
  if (!file) { console.error("usage: ownership-recover.mjs <owner.json> [--apply] [--wait-ms N]"); process.exit(64); }
  recover({ file, apply: args.includes("--apply"), waitMs: waitAt >= 0 ? Number(args[waitAt + 1]) : undefined }).then((code) => process.exit(code), (e) => { console.error(`recover: ${e.stack ?? e}`); process.exit(2); });
}
