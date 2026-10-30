// The content digest of an npm archive: sha256 over the sorted list of (path, type, exec bit, sha256 of the content) of its entries.
// The archive's own bytes depend on gzip, entry order and mtimes; this digest depends only on what the package ships, so two packs of
// one commit compare equal whatever produced them. It is read with node:zlib and a strict ustar/pax reader (no system tar: the listing
// `tar -tzf` carries no mode or type, and the hosted runner and the local host must agree on one reading).
//
// Exclusions are code constants, never input (rightkit#1301 B2): the one binary whose bytes differ between a local pack and the hosted
// one is @rightkit/release's Developer-ID-signed `bin/darwin/rightkit-swap`, which is re-signed with a timestamp on every signing, and
// its `.sign.json` receipt. `exclusionsFor` returns them for that one package; every other unit has an empty set. The hosted side binds
// the excluded helper separately (publish-admission.mjs bindPackedToQualification: its sha256 equals the sign-swap job's).
import { createHash } from "node:crypto";
import fs from "node:fs";
import zlib from "node:zlib";
import { canonicalJson } from "./provenance-verify.mjs";
import { SWAP_HELPER_BIN, SWAP_HELPER_PACKAGE, SWAP_HELPER_RECEIPT } from "./swap-helper-bundle.mjs";

export const CONTENT_DIGEST_DOMAIN = "rightkit-npm-content-v1";
/** An npm tarball's entries all live under `package/`. */
const ROOT = "package";
export const SWAP_HELPER_ARCHIVE_BIN = `${ROOT}/${SWAP_HELPER_BIN.join("/")}`;
export const SWAP_HELPER_ARCHIVE_RECEIPT = `${ROOT}/${SWAP_HELPER_RECEIPT.join("/")}`;

/** The archive paths left out of a unit's content digest: the swap helper and its receipt for @rightkit/release, nothing for any other unit. */
export function exclusionsFor(unitName) {
  return unitName === SWAP_HELPER_PACKAGE ? [SWAP_HELPER_ARCHIVE_BIN, SWAP_HELPER_ARCHIVE_RECEIPT] : [];
}

const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const MAX_ARCHIVE_BYTES = 1024 * 1024 * 1024;
const TYPES = { 0: "file", "\0": "file", 1: "hardlink", 2: "symlink", 5: "directory" };

const field = (block, start, length) => { const end = block.indexOf(0, start); return block.toString("utf8", start, end === -1 || end > start + length ? start + length : end); };
const octal = (block, start, length, what) => {
  // A size or mode field is octal ASCII, or base-256 (high bit of the first byte) for a size beyond 8 GiB.
  if (block[start] & 0x80) { let n = 0n; for (let i = 1; i < length; i += 1) n = (n << 8n) | BigInt(block[start + i]); return Number(n); }
  const text = block.toString("ascii", start, start + length).replace(/[\0 ]+$/g, "").trim();
  if (text === "") return 0;
  if (!/^[0-7]+$/.test(text)) throw new Error(`the archive has a malformed ${what} field`);
  return Number.parseInt(text, 8);
};

function paxRecords(data) {
  const out = {};
  let at = 0;
  while (at < data.length) {
    const space = data.indexOf(0x20, at);
    const length = Number.parseInt(data.toString("ascii", at, space), 10);
    if (!Number.isInteger(length) || space < 0 || length < space - at + 2 || at + length > data.length) throw new Error("the archive has a malformed pax header");
    const record = data.toString("utf8", space + 1, at + length - 1);
    const eq = record.indexOf("=");
    if (eq > 0) out[record.slice(0, eq)] = record.slice(eq + 1);
    at += length;
  }
  return out;
}

/** The entries of a gunzipped tar: `[{ path, type, mode, data }]`, in archive order. Refuses what it cannot read exactly. */
export function parseTar(tar) {
  const entries = [];
  let at = 0;
  let pending = {}; // pax/GNU overrides for the next entry
  let ended = false;
  while (at + 512 <= tar.length) {
    const header = tar.subarray(at, at + 512);
    if (header.every((b) => b === 0)) { ended = true; break; }
    const stored = octal(header, 148, 8, "checksum");
    let sum = 0;
    for (let i = 0; i < 512; i += 1) sum += i >= 148 && i < 156 ? 0x20 : header[i];
    if (sum !== stored) throw new Error("the archive has a header with a bad checksum");
    const size = octal(header, 124, 12, "size");
    const flag = String.fromCharCode(header[156]);
    const bodyStart = at + 512;
    const bodyEnd = bodyStart + size;
    if (bodyEnd > tar.length) throw new Error("the archive is truncated");
    const body = tar.subarray(bodyStart, bodyEnd);
    at = bodyStart + Math.ceil(size / 512) * 512;
    if (flag === "x") {
      const records = paxRecords(body);
      if (records.size !== undefined) throw new Error("the archive overrides an entry size in a pax header");
      pending = { ...pending, ...records };
      continue;
    }
    if (flag === "g") throw new Error("the archive carries a global pax header");
    if (flag === "L") { pending.path = body.toString("utf8").replace(/\0+$/, ""); continue; }
    if (flag === "K") { pending.linkpath = body.toString("utf8").replace(/\0+$/, ""); continue; }
    const type = TYPES[flag];
    if (!type) throw new Error(`the archive has an entry of unsupported type ${JSON.stringify(flag)}`);
    const ustar = header.toString("ascii", 257, 262) === "ustar";
    const prefix = ustar ? field(header, 345, 155) : "";
    const name = pending.path ?? (prefix ? `${prefix}/${field(header, 0, 100)}` : field(header, 0, 100));
    const linkpath = pending.linkpath ?? field(header, 157, 100);
    pending = {};
    entries.push({ path: name.replace(/\/+$/, ""), type, mode: octal(header, 100, 8, "mode"), data: type === "file" ? body : type === "directory" ? Buffer.alloc(0) : Buffer.from(linkpath, "utf8") });
  }
  if (!ended) throw new Error("the archive is truncated: it has no end marker");
  if (Object.keys(pending).length) throw new Error("the archive ends after a header with no entry");
  return entries;
}

/**
 * `{ contentSha256, entries, excluded }` of an npm archive. `exclude` lists archive paths (`package/...`) left out of the digest; each one
 * present is reported in `excluded` as `{ type, exec, sha256 }`, so a caller binds it by another route. Two entries of one path, an
 * unreadable header or an unsupported entry type refuse (an archive that reads two ways has no one digest).
 */
export function npmContentDigest(tgz, { exclude = [] } = {}) {
  let tar;
  try { tar = zlib.gunzipSync(Buffer.isBuffer(tgz) ? tgz : fs.readFileSync(tgz), { maxOutputLength: MAX_ARCHIVE_BYTES }); }
  catch (error) { throw new Error(`the archive is not a readable gzip (${error.message})`); }
  const skip = new Set(exclude);
  const rows = [];
  const excluded = {};
  const seen = new Set();
  for (const entry of parseTar(tar)) {
    if (seen.has(entry.path)) throw new Error(`the archive holds ${entry.path} twice`);
    seen.add(entry.path);
    const row = { path: entry.path, type: entry.type, exec: (entry.mode & 0o111) !== 0, sha256: sha256(entry.data) };
    if (skip.has(entry.path)) excluded[entry.path] = { type: row.type, exec: row.exec, sha256: row.sha256 };
    else rows.push(row);
  }
  rows.sort((a, b) => Buffer.compare(Buffer.from(a.path, "utf8"), Buffer.from(b.path, "utf8")));
  const contentSha256 = sha256(`${CONTENT_DIGEST_DOMAIN}\n${canonicalJson(rows.map((r) => [r.path, r.type, r.exec, r.sha256]))}`);
  return { contentSha256, entries: rows.length, excluded };
}
