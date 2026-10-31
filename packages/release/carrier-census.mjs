// GB-76: owner-aware retirement of the snapshot-request carriers a crashed gate left behind (BR16, B34; BROKER-CONTRACT section 16).
//
// A carrier is written under `<control root>/snapshot-requests/<name>.json` and removed by its caller's `finally`; a gate that crashed between the write, the send and the cleanup left the file.
// RustRight's wire for the creator record: BEFORE the carrier the gate writes `<name>.json.owner.json`
//   { "schema": "rustright.carrier-owner/1", "gate": { "pid", "startTimeMs" }, "nonce", "handle", "createdAtMs" }
// (its own process identity as RustRight reads it, the call's attestation nonce and ownership handle, the creation time in UNIX epoch milliseconds), then the carrier, and removes both in its `finally`.
// The broker's maintenance pass (`carrier_sweep::sweep_carriers`) retires a carrier only when its owner record parses and names this schema, its gate identity is read as gone or reused (a live gate
// keeps its carrier until the call has been finished for an hour, an unreadable identity keeps it for ever) and the broker holds no active or unknown submission under its handle. Age alone never
// retires anything. `sweepSnapshotRequests` is the gate's own sweep over the same wire and the same rules: nothing is deleted without proof, every entry is accounted for, `grant-*.json` and the
// owner records themselves are never carriers, links are never followed, and removal is compare-token (the owner record and the carrier must still be byte-identical to what was judged).
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { processStartTimeMs, selfStartTimeMs } from "./lock-identity.mjs";

export const CARRIER_OWNER_SCHEMA = "rustright.carrier-owner/1";
export const CARRIER_DIR = "snapshot-requests";
/** A carrier whose call has been finished this long is retired even when its gate lives, when the broker holds nothing under it (RustRight FINISHED_CARRIER_RETENTION_MS). */
export const FINISHED_CARRIER_RETENTION_MS = 60 * 60 * 1000;
/** A carrier of a dead gate that the broker never saw stays this long before the gate retires it (the client the gate started may still be finishing). */
export const CARRIER_RETENTION_MS = 15 * 60 * 1000;
const MAX_CARRIER_BYTES = 4 * 1024 * 1024;
const MAX_OWNER_BYTES = 64 * 1024;
const OWNER_SUFFIX = ".owner.json";
const OWNER_KEYS = ["createdAtMs", "gate", "handle", "nonce", "schema"];
const digest = (data) => crypto.createHash("sha256").update(data).digest("hex");
/** The owner record's file: the carrier's own name, extension included, then `.owner.json`. */
export const ownerFileOf = (carrierFile) => `${carrierFile}${OWNER_SUFFIX}`;

/**
 * Writes the creator record for the carrier at `file` (owner-only, exclusive, BEFORE the carrier itself) and returns it. `identity()` is `{ pid, startTimeMs }` (startTimeMs null when this host cannot
 * read it: the broker then keeps the carrier for ever, which is safe).
 */
export function writeCarrierOwner({ file, handle, nonce, identity = () => ({ pid: process.pid, startTimeMs: selfStartTimeMs() }), now = Date.now() }) {
  const me = identity();
  const record = { schema: CARRIER_OWNER_SCHEMA, gate: { pid: me.pid, startTimeMs: me.startTimeMs ?? null }, nonce, handle, createdAtMs: now };
  fs.writeFileSync(ownerFileOf(file), JSON.stringify(record), { flag: "wx", mode: 0o600 });
  return record;
}

/** Removes a carrier and its owner record after its call (the normal path: nothing is left for a sweep). */
export function removeCarrier(file) {
  fs.rmSync(file, { force: true });
  fs.rmSync(ownerFileOf(file), { force: true });
}

const readBounded = (file, max) => {
  const meta = fs.lstatSync(file);
  if (meta.isSymbolicLink() || !meta.isFile()) return { problem: "not a regular file" };
  if (meta.size > max) return { problem: `larger than ${max} bytes` };
  return { bytes: fs.readFileSync(file) };
};

/** The owner record `bytes` hold, exactly RustRight's shape (it denies unknown fields), or `{ problem }`. */
function parseOwner(bytes) {
  let value; try { value = JSON.parse(bytes.toString("utf8")); } catch { return { problem: "not JSON" }; }
  const object = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  if (!object(value) || Object.keys(value).sort().join() !== OWNER_KEYS.join()) return { problem: "not the fields of rustright.carrier-owner/1" };
  if (value.schema !== CARRIER_OWNER_SCHEMA) return { problem: "not a rustright.carrier-owner/1 record" };
  if (!object(value.gate) || Object.keys(value.gate).sort().join() !== "pid,startTimeMs" || !Number.isInteger(value.gate.pid) || value.gate.pid < 1 || value.gate.pid > 0xffffffff || !(value.gate.startTimeMs === null || (Number.isSafeInteger(value.gate.startTimeMs) && value.gate.startTimeMs >= 0))) return { problem: "the gate identity is not {pid, startTimeMs}" };
  if (typeof value.nonce !== "string" || value.nonce === "" || typeof value.handle !== "string" || value.handle === "" || !Number.isSafeInteger(value.createdAtMs) || value.createdAtMs < 0) return { problem: "nonce, handle or createdAtMs is not valid" };
  return { owner: value };
}

/** Whether the gate a record names is the same process, gone, reused (the pid runs a process that started at another time) or unreadable, as RustRight's `identity_state` judges it. */
export function gateState(gate, probe) {
  let seen; try { seen = probe(gate.pid); } catch { return "unverifiable"; }
  if (!seen?.alive) return "gone";
  if (gate.startTimeMs === null) return "unverifiable";
  if (!Number.isFinite(seen.startTimeMs)) return "unverifiable";
  return seen.startTimeMs === gate.startTimeMs ? "same" : "reused";
}

const livePid = (pid) => { try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; } };
const hostProbe = (pid) => (livePid(pid) ? { alive: true, startTimeMs: pid === process.pid ? selfStartTimeMs() : processStartTimeMs(pid) } : { alive: false, startTimeMs: null });

/**
 * Sweeps one carrier directory. `lease({ snapshot, handle })` is the broker's lease status (build-ownership.mjs brokerLeaseState) and must answer for the exact sub-handle; `identity.probe(pid)`
 * is `{ alive, startTimeMs }`. Returns `{ retired: [file], retained: [{ file, why }], corrupt: [{ file, why }], examined }`: every entry is accounted for; nothing is deleted without proof.
 */
export function sweepSnapshotRequests({ controlRoot, lease, snapshot = null, now = Date.now(), retentionMs = CARRIER_RETENTION_MS, limit = 256, identity = { probe: hostProbe } }) {
  const dir = path.join(controlRoot, CARRIER_DIR);
  const out = { retired: [], retained: [], corrupt: [], examined: 0 };
  let dirMeta; try { dirMeta = fs.lstatSync(dir); } catch (e) { if (e.code === "ENOENT") return out; throw e; }
  if (dirMeta.isSymbolicLink() || !dirMeta.isDirectory()) { out.corrupt.push({ file: dir, why: "the carrier directory is not a real directory" }); return out; }
  const names = fs.readdirSync(dir).filter((n) => n.endsWith(".json") && !n.endsWith(OWNER_SUFFIX) && !n.startsWith(".") && !n.startsWith("grant-")).sort().slice(0, limit);
  for (const name of names) {
    const file = path.join(dir, name);
    out.examined += 1;
    const keep = (why) => out.retained.push({ file, why });
    const corrupt = (why) => out.corrupt.push({ file, why });
    let carrier; let ownerBytes;
    try {
      carrier = readBounded(file, MAX_CARRIER_BYTES);
      if (carrier.problem) { corrupt(`carrier ${carrier.problem}`); continue; }
      if (!fs.lstatSync(ownerFileOf(file), { throwIfNoEntry: false })) { keep("no owner record: its creator cannot be known"); continue; }
      const o = readBounded(ownerFileOf(file), MAX_OWNER_BYTES);
      if (o.problem) { corrupt(`owner record ${o.problem}`); continue; }
      ownerBytes = o.bytes;
    } catch (e) { corrupt(`unreadable (${e.code ?? e.message})`); continue; }
    const parsed = parseOwner(ownerBytes);
    if (parsed.problem) { corrupt(`the owner record is not one this gate wrote (${parsed.problem})`); continue; }
    const { owner } = parsed;
    const gate = gateState(owner.gate, identity.probe);
    if (gate === "unverifiable") { keep("its gate's identity cannot be read, so it cannot be proven dead"); continue; }
    const finished = now - owner.createdAtMs >= FINISHED_CARRIER_RETENTION_MS;
    if (gate === "same" && !finished) { keep("its gate is alive"); continue; }
    let doc = null; try { doc = JSON.parse(carrier.bytes.toString("utf8")); } catch { /* the snapshot comes from the broker's view below */ }
    const answer = lease({ snapshot: doc?.snapshot?.path ?? snapshot, handle: owner.handle });
    const members = Array.isArray(answer.members) ? answer.members : null;
    const old = now - owner.createdAtMs >= retentionMs;
    const settled = answer.state === "REAPED" || (answer.state === "UNKNOWN" && members !== null && members.length === 0 && old);
    if (!settled) { keep(answer.state === "ACTIVE" ? "the broker holds it ACTIVE" : answer.state === "UNKNOWN" && members?.length ? "the broker knows it UNKNOWN" : `the broker's answer (${answer.state}) does not settle it${old ? "" : " and it is younger than the retention"}`); continue; }
    // Compare-token removal: the carrier and the owner record are exactly what was judged, and still not links.
    try {
      const again = readBounded(file, MAX_CARRIER_BYTES); const ownerAgain = readBounded(ownerFileOf(file), MAX_OWNER_BYTES);
      if (again.problem || ownerAgain.problem || digest(again.bytes) !== digest(carrier.bytes) || digest(ownerAgain.bytes) !== digest(ownerBytes)) { keep("changed while it was judged"); continue; }
      fs.rmSync(file, { force: true });
      fs.rmSync(ownerFileOf(file), { force: true });
      out.retired.push(file);
    } catch (e) { keep(`not removed (${e.code ?? e.message})`); }
  }
  return out;
}
