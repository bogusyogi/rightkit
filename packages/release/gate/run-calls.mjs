// Runs the call list of one run (design sections 6, 9 and 17). One call after another, because the broker runs one Cargo process per app.
//
// Three rules and nothing else:
//   1. Skip: a call that already passed for this exact commit, command, toolchain, host, broker and gate version is not sent again.
//      The key has no per-crate input. Of the environment it carries exactly the variables the broker passes on to Cargo that can change a
//      result (CALL_KEY_ENV); the whole environment in a key was the cache-miss loop. A call is remembered only when its signed receipt shows
//      the broker forwarded nothing outside the names this file knows, so a broker that starts forwarding more cannot be keyed too narrowly.
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

/**
 * The variables the broker passes on to Cargo (rustright-policy environment.rs MANAGED_REQUEST_ENVIRONMENT_KEYS, read 2026-11-17), in two parts.
 * CALL_KEY_ENV can change what is built or linted and is in the key. CALL_DISPLAY_ENV only changes how output looks and is not: TERM in a key
 * makes every terminal a miss. The broker refuses any other name, and forwards cargo-ndk's compiler entries only when an NDK root is set; a
 * call that forwarded one of those, or any name not listed here, is never remembered (see `unrememberable`).
 */
// RUSTDOCFLAGS is the broker's one value-pinned extra: it admits exactly "-D warnings", on doc calls only, and only when the request carries it.
// It is in the key, so a doc call that passed without it is never reused for one sent with it.
export const CALL_KEY_ENV = Object.freeze(["ANDROID_NDK_HOME", "CI", "IPHONEOS_DEPLOYMENT_TARGET", "NDK_HOME", "RUSTDOCFLAGS", "RUST_BACKTRACE", "RUST_LOG", "SOURCE_DATE_EPOCH", "TZ"]);
export const CALL_DISPLAY_ENV = Object.freeze(["CARGO_TERM_COLOR", "NO_COLOR", "TERM", "__CF_USER_TEXT_ENCODING"]);

/** The names the broker forwarded for `job`, from its signed receipt (`environment.forwarded`); null when the receipt does not say. */
export function forwardedOf(job, { readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  try { const f = JSON.parse(readFile(job.receipt)).environment?.forwarded; return Array.isArray(f) && f.every((n) => typeof n === "string") ? f : null; } catch { return null; }
}
/**
 * Whether the output the client handed back for `job` is all of it. The broker drops the middle of a large job's output and says how much in its
 * receipt (`outputDroppedBytes`). The job log the receipt names (`diagnosticsPath`) is written before anything is dropped, but it is usable as the
 * whole output only when the receipt shows it is complete and it can be read as Cargo's records (rustright-broker job.rs, read 2026-11-17):
 *   - `output.log.truncated` is false (the log has its own cap), `output.log.bytes` equals `output.bytes`, and the file holds at least that many
 *     bytes; only the first `output.log.bytes` bytes are read (late output is appended after them);
 *   - the log holds stdout and stderr interleaved chunk by chunk with no tag, so a Cargo record can have other text written into it. A line that
 *     carries the start of a record and is not valid JSON means records cannot be trusted from this log: it is not repaired, it is refused.
 * A newer broker also keeps the first stream alone (`output.stdout.log`); that file is preferred and needs no such refusal (see below).
 * Returns { dropped: <bytes, 0 when none, null when the receipt does not say>, full: <the whole output, or null>, why: <why there is no full output> }.
 */
export function fullOutputOf(job, { readFile = (f) => fs.readFileSync(f, "utf8"), readBytes = (f) => fs.readFileSync(f) } = {}) {
  let receipt; try { receipt = JSON.parse(readFile(job.receipt)); } catch { return { dropped: null, full: null, why: "the receipt cannot be read" }; }
  const dropped = Number.isSafeInteger(receipt?.outputDroppedBytes) && receipt.outputDroppedBytes >= 0 ? receipt.outputDroppedBytes : null;
  if (!dropped) return { dropped, full: null, why: null };
  const no = (why) => ({ dropped, full: null, why });
  // A broker that keeps a log of the first output stream alone says so in `output.stdout` (rustright GB-100). That file holds Cargo's records whole
  // and in order, so no record can have other text in it. When the receipt has `output.stdout` it is the only log used: whole, or the result is unknown.
  const so = receipt.output?.stdout;
  if (so !== undefined && so !== null) {
    const sl = so.log;
    if (!sl || typeof sl.path !== "string" || !sl.path) return no("the broker kept no log of the build's own output stream");
    if (sl.truncated !== false) return no("the log of the build's own output stream was cut at its size limit");
    if (!Number.isSafeInteger(sl.bytes) || sl.bytes !== so.bytes) return no("the log of the build's own output stream does not hold every byte of it");
    let sb; try { sb = readBytes(sl.path); } catch { return no("the log of the build's own output stream cannot be read (logs are removed after 7 days)"); }
    if (sb.length < sl.bytes) return no("the log of the build's own output stream is shorter on disk than the receipt says");
    return { dropped, full: Buffer.from(sb).subarray(0, sl.bytes).toString("utf8"), why: null };
  }
  const log = receipt.output?.log;
  if (typeof receipt.diagnosticsPath !== "string" || !receipt.diagnosticsPath || !log) return no("the receipt names no job log");
  if (log.truncated !== false) return no("the job log was itself cut at its size limit");
  if (!Number.isSafeInteger(log.bytes) || log.bytes !== receipt.output?.bytes) return no("the job log does not hold every byte the job wrote");
  let bytes; try { bytes = readBytes(receipt.diagnosticsPath); } catch { return no("the job log cannot be read (logs are removed after 7 days)"); }
  if (bytes.length < log.bytes) return no("the job log on disk is shorter than the receipt says");
  const full = Buffer.from(bytes).subarray(0, log.bytes).toString("utf8");
  for (const raw of full.split(/\r?\n/)) {
    if (!raw.includes('{"reason":')) continue;
    try { JSON.parse(raw.trim()); } catch { return no("the job log has other output written into one of Cargo's records (the log mixes both output streams)"); }
  }
  return { dropped, full, why: null };
}

/** The build of the broker that ran `job`, from the signed part of its receipt (`provenance.payload.broker.brokerDigest`); null when the receipt does not say. */
export function signedBrokerOf(job, { readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  try { const d = JSON.parse(readFile(job.receipt)).provenance?.payload?.broker?.brokerDigest; return typeof d === "string" && d ? d : null; } catch { return null; }
}
/** Why a passed job must not be remembered, or null. Fails closed: no statement of what was forwarded is a reason. */
export function unrememberable(forwarded) {
  if (!Array.isArray(forwarded)) return "its receipt does not say which environment variables the broker forwarded";
  const odd = forwarded.filter((n) => !CALL_KEY_ENV.includes(n) && !CALL_DISPLAY_ENV.includes(n));
  return odd.length ? `the broker forwarded ${odd.slice(0, 6).join(", ")}${odd.length > 6 ? `, +${odd.length - 6} more` : ""}, which the remembered-call key does not carry` : null;
}

/** The skip key of a call. `identity`: { commit, toolchain, host, broker, gate } (every field a string; a missing one refuses the run). `env`: the environment the call is sent with. */
export function callKey(call, identity, env = {}) {
  for (const field of ["commit", "toolchain", "host", "broker", "gate"]) {
    if (typeof identity?.[field] !== "string" || !identity[field]) throw new TypeError(`callKey: identity.${field} is missing; a call cannot be remembered without it`);
  }
  return sha256(JSON.stringify({ v: 2, commit: identity.commit, toolchain: identity.toolchain, host: identity.host, broker: identity.broker, gate: identity.gate, cwd: call.cwd ?? ".", args: call.args, env: CALL_KEY_ENV.map((n) => [n, typeof env?.[n] === "string" ? env[n] : null]) }));
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

const refusalOf = (sent) => (sent.refusal && sent.refusal.code !== "EXIT" ? sent.refusal : null);
const allPassed = (results) => results.length > 0 && results.every((r) => r.result === "passed");
const jobOf = (sent) => ({ exit: sent.status ?? null, requestId: sent.requestId ?? null, receipt: sent.receipt ?? null, receiptSha256: sent.receiptSha256 ?? null });

/**
 * Runs `calls` in order.
 * `send(call)`: sends one call and returns { status, stdout, stderr, requestId?, receipt?, receiptSha256?, refusal?: { code, message } }
 *   (seam-broker.mjs runSeamCargoCall's `result` on the Mac and the Dell; the direct wrapper on Linux).
 * `store`: fileStore(dir) or null (nothing remembered). `identity`: see callKey. `hostTriple`: this host's target.
 * `output(job)`: see fullOutputOf. When the broker dropped part of a job's output, the crates are judged from the whole job log; when that log
 *   cannot be read, no crate is passed or called not built from the partial stream (a crate with an error that was seen still failed).
 * `signedBroker(job)`: see signedBrokerOf. A pass is remembered only when the job's signed receipt names the broker build the key names.
 * `env`: the environment `send` sends a call with, or a function (call) => that environment when calls differ (a doc call carries RUSTDOCFLAGS).
 *   Its CALL_KEY_ENV values are in the key, read before the store is looked up. `forwarded(job)`: see forwardedOf.
 * Returns { outcomes: [{ call, state: "skipped"|"ran", results, jobs, evidence, notRemembered? }], counts: { calls, ran, skipped, jobs, greenReruns, notRemembered } }.
 * `evidence` is the job that proves each passed crate: for a clean call its one job, after a green rerun the second job.
 */
export function runCalls({ calls, send, store = null, identity, hostTriple, onCall = () => {}, env = {}, forwarded = forwardedOf, output = fullOutputOf, signedBroker = signedBrokerOf }) {
  const outcomes = [];
  const counts = { calls: calls.length, ran: 0, skipped: 0, jobs: 0, greenReruns: 0, notRemembered: 0 };
  const envOf = (which) => (typeof env === "function" ? env(which) : env);
  let brokerChanged = false;
  // Remembers a passed job, unless its receipt shows the key is too narrow for what the broker forwarded. Returns the reason it was not remembered, or null.
  const remember = (which, job) => {
    if (!store) return null;
    // The key names a broker build; the pass is filed under it only when the job's own signed receipt says that build ran it.
    const ranBy = signedBroker(job, identity);
    if (ranBy !== identity.broker) { counts.notRemembered += 1; brokerChanged = true; return `its receipt says broker ${ranBy ?? "(not stated)"} ran it, the key names ${identity.broker}`; }
    const why = unrememberable(forwarded(job));
    if (why) { counts.notRemembered += 1; return why; }
    store.put(callKey(which, identity, envOf(which)), { call: { id: which.id, cwd: which.cwd ?? ".", args: which.args }, job, crates: which.crates.map((c) => c.name) });
    return null;
  };
  const judge = (call, sent0) => {
    // The stream the client handed back may have a hole in it. Judge from the whole job log, or say plainly that the result is not known.
    let sent = sent0; let hole = null;
    if (sent0.receipt) {
      const o = output(jobOf(sent0));
      if (o.dropped > 0 && typeof o.full === "string") sent = { ...sent0, stdout: o.full };
      else if (o.dropped > 0) hole = `the broker dropped ${o.dropped} bytes of this job's output and its log cannot stand in for it (${o.why}); this crate's result is not known`;
    }
    const judged = judgeWhole(call, sent);
    return hole ? judged.map((r) => (r.result === "failed" ? r : { crate: r.crate, result: "unknown", detail: hole })) : judged;
  };
  const judgeWhole = (call, sent) => {
    // The seam marks every non-passing call with a code. "EXIT" means Cargo ran and exited non-zero: that is a build result to read, not a refusal
    // (Dell planted-failure run, 2026-11-10: treating it as a refusal made every crate of a red call unknown and sent no green rerun).
    const refusal = refusalOf(sent) ? `${sent.refusal.code}: ${sent.refusal.message}` : null;
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
    const key = callKey(call, identity, envOf(call));
    const remembered = store?.get(key) ?? null;
    let notRemembered = null; brokerChanged = false;
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
    if (allPassed(results) && first.status === 0 && !refusalOf(first)) {
      evidence = jobs[0];
      notRemembered = remember(call, evidence);
    } else {
      const green = results.filter((r) => r.result === "passed").map((r) => r.crate);
      const reduced = green.length ? restrictCall(call, green) : null;
      if (reduced && reduced !== call && reduced.crates.length < call.crates.length) {
        counts.jobs += 1; counts.greenReruns += 1;
        const second = send(reduced);
        const again = judge(reduced, second);
        jobs.push(jobOf(second));
        const proven = allPassed(again) && second.status === 0 && !refusalOf(second);
        if (proven) {
          evidence = jobs[1];
          notRemembered = remember(reduced, evidence);
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
    const outcome = { call, state: "ran", results, jobs, evidence, ...(notRemembered ? { notRemembered } : {}), ...(brokerChanged ? { brokerChanged: true } : {}) };
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
