// The gate-owned ledger of every call the runtime cargo wrapper decides (item 7 rows 7.10 and 7.24, Sage decision C).
//
// The wrapper (cargo-target-shim.mjs) appends one record BEFORE it acts (`pending`) and one after (`ran`, `verified`, `unverified` or
// `refused`), to `<ledger dir>/<scan token>.jsonl`. The gate (runScans) reads the file of each scan after it ended and fails the scan
// on anything but a completed, successful call: a refused call fails its unit even when the test swallowed cargo's exit status, and a
// call that never completed (the wrapper was killed, or the record was never finished) is unverified, never skipped. Records the gate
// cannot attribute to a scan (the scan token was cleared) fail the run as a row of their own.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const LEDGER_SCHEMA = "rightkit.cargo-ledger/1";
export const SCAN_TOKEN_ENV = "RIGHTKIT_CARGO_SCAN";
export const UNSCOPED = "unscoped";
/** Statuses that are a completed success: a direct call that ran, a broker call whose signed receipt verified. */
export const OK_STATUSES = Object.freeze(["ran", "verified"]);

/** The token a scan's calls are filed under: stable per scan id, never the id itself. */
export const scanTokenOf = (scanId) => crypto.createHash("sha256").update(String(scanId)).digest("hex").slice(0, 16);

const tokenFile = (dir, token) => path.join(dir, `${token}.jsonl`);
const validToken = (token) => (typeof token === "string" && /^[0-9a-f]{16}$/.test(token) ? token : UNSCOPED);

/** Appends one record. Throws when it cannot be written: a call that cannot be recorded does not run. */
export function appendLedger(dir, token, entry) {
  if (!dir || !path.isAbsolute(dir)) throw new Error("the cargo ledger directory is not set");
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const fd = fs.openSync(tokenFile(dir, validToken(token)), fs.constants.O_WRONLY | fs.constants.O_APPEND | fs.constants.O_CREAT | (fs.constants.O_NOFOLLOW ?? 0), 0o600);
  try { fs.writeSync(fd, `${JSON.stringify({ schema: LEDGER_SCHEMA, at: new Date().toISOString(), ...entry })}\n`); } finally { fs.closeSync(fd); }
}

/**
 * The calls filed under `token`, one per call id (a later record replaces an earlier one), and the problems found: an unreadable
 * line, and every call that is not a completed success.
 */
export function readLedger(dir, token) {
  const file = tokenFile(dir, validToken(token));
  let text = "";
  try { text = fs.readFileSync(file, "utf8"); } catch (e) { if (e.code === "ENOENT") return { entries: [], problems: [] }; throw e; }
  const byId = new Map(); const problems = [];
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let entry;
    try { entry = JSON.parse(line); } catch { problems.push(`an unreadable ledger record in ${path.basename(file)}`); continue; }
    if (entry?.schema !== LEDGER_SCHEMA || typeof entry.id !== "string") { problems.push(`a malformed ledger record in ${path.basename(file)}`); continue; }
    byId.set(entry.id, { ...(byId.get(entry.id) ?? {}), ...entry });
  }
  const entries = [...byId.values()];
  for (const e of entries) {
    if (OK_STATUSES.includes(e.status)) continue;
    const call = `cargo ${(e.argv ?? []).join(" ")}`.trim();
    problems.push(e.status === "pending" ? `${call} (from ${e.cwd}) never completed: the wrapper recorded it and no outcome, so it is unverified` : `${call} (from ${e.cwd}) was ${e.status}${e.code ? ` (${e.code})` : ""}${e.message ? `: ${e.message}` : ""}`);
  }
  return { entries, problems };
}

/** Ledger files nobody claimed: the scan token was cleared or invented by the code under test. */
export function unclaimedLedgers(dir, claimedTokens) {
  let names = [];
  try { names = fs.readdirSync(dir); } catch (e) { if (e.code === "ENOENT") return []; throw e; }
  return names.filter((n) => n.endsWith(".jsonl")).map((n) => n.slice(0, -".jsonl".length)).filter((t) => !claimedTokens.has(t));
}
