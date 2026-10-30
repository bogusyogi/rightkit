// GB-76: owner-aware retirement of the snapshot-request carriers a crashed gate left behind (BR16, B34).
//
// A carrier is written under `<control root>/snapshot-requests/<uuid>.json` and removed by its caller's `finally`; a gate that crashed between the write, the send and the cleanup
// left the file. The carrier's own fields are fixed by RustRight's reader, so its creator is recorded beside it in `<uuid>.owner.json`
// ({ schema, pid, startedMs, bootId, host, handle, nonce, sha256, createdAt }): who made it, which sub-handle it carries, its digest. `sweepSnapshotRequests` retires a carrier only on proof,
// never on age alone and never by excluding names:
//   - the creator is PROVEN dead (the lock holder judge: gone pid, a pid reused by a process started at another time, a reboot; another host or an unreadable identity is unknown), and
//   - the broker holds nothing live or unknown under its sub-handle: REAPED (a completed or refused submission), or UNKNOWN with no members AND the carrier older than the retention
//     (a dead creator can send nothing more; the retention covers a client it started that is still finishing), and
//   - removal is by token: the file is a regular file (lstat, no link followed), at most 4 MiB, its digest equals the one the owner record holds, and it is unchanged when removed.
// ACTIVE and UNKNOWN-with-members carriers are retained and counted, as are carriers of live creators, other hosts, unreadable owners, corrupt or oversize entries and entries with no owner
// record (kept, counted, never guessed at). The sweep is bounded (`limit` entries) and never follows a link.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { LOCK_SAME_START_MS, lockBootId, lockProcessIdentity, selfLockIdentity } from "./lock-identity.mjs";
import { judgeLockHolder } from "./ownership-mark.mjs";

export const CARRIER_OWNER_SCHEMA = "rightkit.snapshot-request-owner/1";
export const CARRIER_DIR = "snapshot-requests";
export const CARRIER_RETENTION_MS = 15 * 60 * 1000;
const MAX_CARRIER_BYTES = 4 * 1024 * 1024;
const MAX_OWNER_BYTES = 64 * 1024;
const sha256 = (data) => crypto.createHash("sha256").update(data).digest("hex");
export const ownerFileOf = (carrierFile) => carrierFile.replace(/\.json$/, ".owner.json");

/** Writes the creator record beside a carrier just written (owner-only, exclusive). Returns the record. */
export function writeCarrierOwner({ file, bytes, handle, nonce, identity = () => selfLockIdentity(), now = Date.now(), host = os.hostname() }) {
  const me = identity();
  const record = { schema: CARRIER_OWNER_SCHEMA, pid: me.pid, startedMs: me.startedMs, bootId: me.bootId ?? null, host, handle, nonce, sha256: sha256(bytes), createdAt: new Date(now).toISOString() };
  fs.writeFileSync(ownerFileOf(file), JSON.stringify(record), { flag: "wx", mode: 0o600 });
  return record;
}

const readBounded = (file, max) => {
  const meta = fs.lstatSync(file);
  if (meta.isSymbolicLink() || !meta.isFile()) return { problem: "not a regular file" };
  if (meta.size > max) return { problem: `larger than ${max} bytes` };
  return { text: fs.readFileSync(file), meta };
};

/**
 * Sweeps one carrier directory. `lease({ snapshot, handle })` is the broker's lease status (build-ownership.mjs brokerLeaseState) and must answer for the exact sub-handle.
 * Returns `{ retired: [file], retained: [{ file, why }], corrupt: [{ file, why }], examined }`: every entry is accounted for; nothing is deleted without proof.
 */
export function sweepSnapshotRequests({ controlRoot, lease, snapshot = null, now = Date.now(), retentionMs = CARRIER_RETENTION_MS, limit = 256, identity, host = os.hostname() }) {
  const dir = path.join(controlRoot, CARRIER_DIR);
  const out = { retired: [], retained: [], corrupt: [], examined: 0 };
  let dirMeta; try { dirMeta = fs.lstatSync(dir); } catch (e) { if (e.code === "ENOENT") return out; throw e; }
  if (dirMeta.isSymbolicLink() || !dirMeta.isDirectory()) { out.corrupt.push({ file: dir, why: "the carrier directory is not a real directory" }); return out; }
  const probe = identity ?? { self: () => selfLockIdentity(), probe: (pid) => lockProcessIdentity(pid), bootId: () => lockBootId() };
  const names = fs.readdirSync(dir).filter((n) => /^[A-Za-z0-9-]+\.json$/.test(n) && !n.endsWith(".owner.json")).sort().slice(0, limit);
  for (const name of names) {
    const file = path.join(dir, name);
    out.examined += 1;
    const keep = (why) => out.retained.push({ file, why });
    let carrier; let owner;
    try {
      const c = readBounded(file, MAX_CARRIER_BYTES);
      if (c.problem) { out.corrupt.push({ file, why: `carrier ${c.problem}` }); continue; }
      carrier = c;
      const o = readBounded(ownerFileOf(file), MAX_OWNER_BYTES);
      if (o.problem === undefined) { try { owner = JSON.parse(o.text); } catch { owner = null; } }
      else if (o.problem === "not a regular file" && !fs.existsSync(ownerFileOf(file))) owner = undefined;
      else { out.corrupt.push({ file, why: `owner record ${o.problem}` }); continue; }
    } catch (e) {
      if (e.code === "ENOENT" && fs.existsSync(file)) owner = undefined; else { out.corrupt.push({ file, why: `unreadable (${e.code ?? e.message})` }); continue; }
    }
    if (owner === undefined || owner === null) { keep(owner === undefined ? "no owner record: its creator cannot be known" : "the owner record is unreadable"); continue; }
    if (owner.schema !== CARRIER_OWNER_SCHEMA || typeof owner.handle !== "string" || !owner.handle.includes("#") || typeof owner.sha256 !== "string") { out.corrupt.push({ file, why: "the owner record is not one this gate wrote" }); continue; }
    if (sha256(carrier.text) !== owner.sha256) { out.corrupt.push({ file, why: "the carrier is not the bytes its owner record names" }); continue; }
    const verdict = judgeLockHolder(owner, { identity: probe, host });
    if (verdict !== "dead") { keep(verdict === "live" ? "its creator is alive" : "its creator cannot be proven dead"); continue; }
    let doc = null; try { doc = JSON.parse(carrier.text); } catch { /* the snapshot comes from the broker's view below */ }
    const answer = lease({ snapshot: doc?.snapshot?.path ?? snapshot, handle: owner.handle });
    const members = Array.isArray(answer.members) ? answer.members : null;
    const old = now - Date.parse(owner.createdAt) >= retentionMs;
    const settled = answer.state === "REAPED" || (answer.state === "UNKNOWN" && members !== null && members.length === 0 && old);
    if (!settled) { keep(answer.state === "ACTIVE" ? "the broker holds it ACTIVE" : answer.state === "UNKNOWN" && members?.length ? "the broker knows it UNKNOWN" : `the broker's answer (${answer.state}) does not settle it${old ? "" : " and it is younger than the retention"}`); continue; }
    // Token-checked removal: unchanged since read, and still not a link.
    try {
      const again = readBounded(file, MAX_CARRIER_BYTES);
      if (again.problem || sha256(again.text) !== owner.sha256) { keep("changed while it was judged"); continue; }
      fs.rmSync(file, { force: true });
      fs.rmSync(ownerFileOf(file), { force: true });
      out.retired.push(file);
    } catch (e) { keep(`not removed (${e.code ?? e.message})`); }
  }
  return out;
}
