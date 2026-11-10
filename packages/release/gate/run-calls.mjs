// Runs the call list of one run (design sections 6, 9 and 17). One call after another, because the broker runs one Cargo process per app.
//
// Three rules and nothing else:
//   1. Skip: a call that already passed for this exact commit, command, toolchain, host, broker and gate version is not sent again.
//      The key has no per-crate input and no environment in it. An unchanged commit therefore reruns as zero broker jobs.
//   2. Result: each crate's result is read from the call's own Cargo output (crate-results.mjs).
//   3. Green rerun: when a call ends with some crates red, the same call is sent once more naming only the crates that passed.
//      That job exits 0 and is the signed evidence for them (CI admits only a job that exited 0); it compiles nothing new.
//      One red crate never voids the others.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { crateResults, readCargoStream } from "./crate-results.mjs";

const sha256 = (text) => crypto.createHash("sha256").update(text).digest("hex");

/** The same call naming only `names` (its `-p` list reduced). A call with no `-p` list (a consumer manifest) is returned unchanged. */
export function restrictCall(call, names) {
  const keep = new Set(names);
  const args = [];
  let had = false;
  for (let i = 0; i < call.args.length; i += 1) {
    if (call.args[i] === "--") { args.push(...call.args.slice(i)); break; }
    if (call.args[i] === "-p") { had = true; if (keep.has(call.args[i + 1])) args.push("-p", call.args[i + 1]); i += 1; continue; }
    args.push(call.args[i]);
  }
  return had ? { ...call, id: `${call.id}:green`, args, crates: call.crates.filter((c) => keep.has(c.name)) } : call;
}

/** The skip key of a call. `identity`: { commit, toolchain, host, broker, gate } (every field a string; a missing one refuses the run). */
export function callKey(call, identity) {
  for (const field of ["commit", "toolchain", "host", "broker", "gate"]) {
    if (typeof identity?.[field] !== "string" || !identity[field]) throw new TypeError(`callKey: identity.${field} is missing; a call cannot be remembered without it`);
  }
  return sha256(JSON.stringify({ v: 1, commit: identity.commit, toolchain: identity.toolchain, host: identity.host, broker: identity.broker, gate: identity.gate, cwd: call.cwd ?? ".", args: call.args }));
}

/** A store of passed calls: one small JSON file per key under `dir`. Only a call whose every crate passed is ever written. */
export function fileStore(dir) {
  const file = (key) => path.join(dir, `${key}.json`);
  return {
    get(key) { try { const v = JSON.parse(fs.readFileSync(file(key), "utf8")); return v?.schema === "rightkit.gate-call/1" && v.key === key ? v : null; } catch { return null; } },
    put(key, record) {
      fs.mkdirSync(dir, { recursive: true });
      const tmp = `${file(key)}.${process.pid}.tmp`;
      fs.writeFileSync(tmp, `${JSON.stringify({ schema: "rightkit.gate-call/1", key, ...record }, null, 1)}\n`);
      fs.renameSync(tmp, file(key));
    },
  };
}

const allPassed = (results) => results.length > 0 && results.every((r) => r.result === "passed");
const jobOf = (sent) => ({ exit: sent.status ?? null, requestId: sent.requestId ?? null, receipt: sent.receipt ?? null, receiptSha256: sent.receiptSha256 ?? null });

/**
 * Runs `calls` in order.
 * `send(call)`: sends one call and returns { status, stdout, stderr, requestId?, receipt?, receiptSha256?, refusal?: { code, message } }
 *   (seam-broker.mjs runSeamCargoCall's `result` on the Mac and the Dell; the direct wrapper on Linux).
 * `store`: fileStore(dir) or null (nothing remembered). `identity`: see callKey. `hostTriple`: this host's target.
 * Returns { outcomes: [{ call, state: "skipped"|"ran", results, jobs, evidence }], counts: { calls, ran, skipped, jobs, greenReruns } }.
 * `evidence` is the job that proves each passed crate: for a clean call its one job, after a green rerun the second job.
 */
export function runCalls({ calls, send, store = null, identity, hostTriple, onCall = () => {} }) {
  const outcomes = [];
  const counts = { calls: calls.length, ran: 0, skipped: 0, jobs: 0, greenReruns: 0 };
  const judge = (call, sent) => {
    const refusal = sent.refusal ? `${sent.refusal.code}: ${sent.refusal.message}` : null;
    if (call.consumer) {
      // A consumer manifest's own package is not one of the named crates: the call is judged whole. It passes for every crate that declares it
      // only when Cargo finished successfully and the job exited 0; any failure in it fails them all, with the errors it reported.
      if (refusal) return call.crates.map((c) => ({ crate: c.name, result: "unknown", detail: refusal }));
      const s = readCargoStream({ stdout: sent.stdout ?? "", stderr: sent.stderr ?? "", targets: [hostTriple], host: hostTriple });
      const ok = s.finished && s.success === true && sent.status === 0;
      const why = ok ? "" : Object.entries(s.errors).slice(0, 4).map(([name, e]) => `${name}: ${e.rendered[0] ?? "error"}`).join(" | ") || (s.finished ? `the consumer call exited ${sent.status}` : "the consumer call produced no complete Cargo output");
      return call.crates.map((c) => ({ crate: c.name, result: ok ? "passed" : s.finished ? "failed" : "unknown", detail: why }));
    }
    return crateResults({ crates: call.crates, stdout: sent.stdout ?? "", stderr: sent.stderr ?? "", exit: sent.status ?? null, refusal, host: hostTriple });
  };
  for (const call of calls) {
    const key = callKey(call, identity);
    const remembered = store?.get(key) ?? null;
    if (remembered) {
      counts.skipped += 1;
      const outcome = { call, state: "skipped", results: call.crates.map((c) => ({ crate: c.name, result: "passed", detail: "" })), jobs: [], evidence: remembered.job };
      outcomes.push(outcome); onCall(outcome); continue;
    }
    counts.ran += 1; counts.jobs += 1;
    const first = send(call);
    let results = judge(call, first);
    const jobs = [jobOf(first)];
    let evidence = null;
    // A job that exited 0 with every crate passed is its own evidence, and the only kind that is remembered.
    if (allPassed(results) && first.status === 0 && !first.refusal) {
      evidence = jobs[0];
      store?.put(key, { call: { id: call.id, cwd: call.cwd ?? ".", args: call.args }, job: evidence, crates: call.crates.map((c) => c.name) });
    } else {
      const green = results.filter((r) => r.result === "passed").map((r) => r.crate);
      const reduced = green.length ? restrictCall(call, green) : null;
      if (reduced && reduced !== call && reduced.crates.length < call.crates.length) {
        counts.jobs += 1; counts.greenReruns += 1;
        const second = send(reduced);
        const again = judge(reduced, second);
        jobs.push(jobOf(second));
        const proven = allPassed(again) && second.status === 0 && !second.refusal;
        if (proven) {
          evidence = jobs[1];
          store?.put(callKey(reduced, identity), { call: { id: reduced.id, cwd: reduced.cwd ?? ".", args: reduced.args }, job: evidence, crates: reduced.crates.map((c) => c.name) });
        } else {
          // The crates looked green in the first call but the call naming only them did not pass: nothing is proven for them.
          const byName = new Map(again.map((r) => [r.crate, r]));
          results = results.map((r) => (r.result !== "passed" ? r : byName.get(r.crate)?.result === "passed" ? { crate: r.crate, result: "unknown", detail: "passed in the together call, but the call naming only the passing crates did not exit 0" } : byName.get(r.crate) ?? r));
        }
      } else if (green.length) {
        // Every crate passed by its artifacts yet the job did not exit 0, or the call cannot be reduced: no signed evidence, so no pass.
        results = results.map((r) => (r.result === "passed" ? { crate: r.crate, result: "unknown", detail: `the job exited ${first.status} with no failure attributed to a crate; nothing is proven` } : r));
      }
    }
    const outcome = { call, state: "ran", results, jobs, evidence };
    outcomes.push(outcome); onCall(outcome);
  }
  return { outcomes, counts };
}

/** Every selected crate's result per stage over the whole run: { crate: { lint, "test-build", docs } }, each { result, detail, evidence: [job...] }. */
export function resultsByCrate(outcomes) {
  const out = {};
  const rank = { failed: 3, "not-built": 2, unknown: 1, passed: 0 };
  for (const o of outcomes) {
    for (const r of o.results) {
      const per = (out[r.crate] ??= {});
      const cur = (per[o.call.stage] ??= { result: "passed", detail: "", evidence: [], calls: [] });
      cur.calls.push({ id: o.call.id, result: r.result });
      if (rank[r.result] > rank[cur.result]) { cur.result = r.result; cur.detail = r.detail; }
      if (r.result === "passed" && o.evidence) cur.evidence.push({ call: o.call.id, ...o.evidence });
    }
  }
  return out;
}
