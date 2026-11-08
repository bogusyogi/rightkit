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
/** Exit status of a refused cargo call, everywhere a gate-made refusal stands in for cargo (cargo's own error status, so every caller reads it as a failed cargo). Defined here,
 * in a module that imports none of the gate's, so the wrapper, the stage seam and the carrier share it without an import cycle reading it before it exists. */
export const CARGO_REFUSAL_STATUS = 101;
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
  try { text = fs.readFileSync(file, "utf8"); } catch (e) { if (e.code === "ENOENT") return { entries: [], problems: [], malformed: 0 }; throw e; }
  const byId = new Map(); const problems = []; let malformed = 0;
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let entry;
    try { entry = JSON.parse(line); } catch { problems.push(`an unreadable ledger record in ${path.basename(file)}`); malformed += 1; continue; }
    if (entry?.schema !== LEDGER_SCHEMA || typeof entry.id !== "string") { problems.push(`a malformed ledger record in ${path.basename(file)}`); malformed += 1; continue; }
    byId.set(entry.id, { ...(byId.get(entry.id) ?? {}), ...entry });
  }
  const entries = [...byId.values()];
  for (const e of entries) {
    if (OK_STATUSES.includes(e.status)) continue;
    const call = `${e.adapter ?? "cargo"} ${(e.argv ?? []).join(" ")}`.trim();
    problems.push(e.status === "pending" ? `${call} (from ${e.cwd}) never completed: the wrapper recorded it and no outcome, so it is unverified` : `${call} (from ${e.cwd}) was ${e.status}${e.code ? ` (${e.code})` : ""}${e.message ? `: ${e.message}` : ""}`);
  }
  // `malformed`: records that could not be read as a call (unreadable JSON, wrong schema, no id). Whose call they were is unknown, so a reader that needs
  // positive evidence (the door's seam lock release) counts each as unsettled.
  return { entries, problems, malformed };
}

/** Every file of a ledger directory read as one ledger (`unscoped` and every scan token), in file-name order: `{ entries, problems, malformed }`. */
export function readLedgerDir(dir) {
  let names = [];
  try { names = fs.readdirSync(dir).filter((n) => n.endsWith(".jsonl")).sort(); } catch (e) { if (e.code === "ENOENT") return { entries: [], problems: [], malformed: 0 }; throw e; }
  const out = { entries: [], problems: [], malformed: 0 };
  for (const name of names) {
    const token = name.slice(0, -".jsonl".length);
    if (token !== UNSCOPED && validToken(token) === UNSCOPED) { out.problems.push(`a ledger file with an invalid name: ${name}`); out.malformed += 1; continue; }
    const r = readLedger(dir, token);
    out.entries.push(...r.entries); out.problems.push(...r.problems); out.malformed += r.malformed;
  }
  return out;
}

/** Ledger files nobody claimed: the scan token was cleared or invented by the code under test. */
export function unclaimedLedgers(dir, claimedTokens) {
  let names = [];
  try { names = fs.readdirSync(dir); } catch (e) { if (e.code === "ENOENT") return []; throw e; }
  return names.filter((n) => n.endsWith(".jsonl")).map((n) => n.slice(0, -".jsonl".length)).filter((t) => !claimedTokens.has(t));
}
