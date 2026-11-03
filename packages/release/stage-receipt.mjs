// Read the stage receipt back and prove it (rightkit#1039, #428).
//
// `stage-sidecar` writes `stage-sidecar.json` (receipt) with one row per staged file: name, path, sha256, and for a cargo-bin row the Cargo profile.
// An app that assembles a payload by hand (copies those files into an .app or a payload folder) must not trust the receipt's word: this module
// reads it back and compares it with the bytes on disk.
//
//   readVerifiedStageReceipt({ root, receiptPath?, target, profile?, mode?, names, kind? }) -> { [name]: { path (absolute), sha256 } }
//     throws StageReceiptError when the receipt is not a valid receipt for `target`, a name has no row (or more than one), a row was staged with another
//     Cargo `profile`, the receipt was written by another build `mode` (a release asks for mode "release"; a receipt that records none is refused),
//     or the file on disk is missing, is not a regular file, or has other bytes than the row.
//
//   assertStageReceiptGuard({ root, config, mode })  the release-time guard, enabled by `targets.<platform>.sign.stageReceipt`:
//     { receipt: "<app-relative receipt>", bind?: { "<app-relative payload file>": "<row name>" } }
//     Immediately before signing: the receipt must be a release receipt (every profiled row `release`), every row's staged file must still have
//     its recorded sha256, and every bound payload file must be byte-identical to its row (a hand-assembled copy that drifted refuses).
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";
import { validateStageSidecarReceipt } from "./stage-sidecar.mjs";

export const DEFAULT_STAGE_RECEIPT = "src-tauri/binaries/stage-sidecar.json";

export class StageReceiptError extends Error {
  constructor(message, details = {}) { super(message); this.name = "StageReceiptError"; this.code = "RK_STAGE_RECEIPT"; Object.assign(this, details); }
}
const fail = (message, details) => { throw new StageReceiptError(message, details); };
const sha256Of = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

function contained(root, relative, label) {
  if (typeof relative !== "string" || !relative || path.isAbsolute(relative) || path.win32.isAbsolute(relative) || /^[A-Za-z]:/.test(relative) || relative.split(/[\\/]/).includes("..")) fail(`${label} must be a relative path inside the app: ${JSON.stringify(relative)}`);
  return path.resolve(root, relative);
}

function loadReceipt({ root, receiptPath, target, fs }) {
  const file = contained(root, receiptPath, "receiptPath");
  let raw;
  try { raw = JSON.parse(String(fs.readFileSync(file, "utf8"))); } catch (error) { fail(`cannot read the stage receipt ${file}: ${error.message}`); }
  try { validateStageSidecarReceipt(raw, target ? { target } : {}); } catch (error) { fail(error.message); }
  return { receipt: raw, file };
}

/** Rows that are staged files themselves (not the per-arch thin slices of a universal row). */
const mainRows = (receipt) => receipt.files.filter((row) => row.sliceOf === undefined);

function verifyRow(root, row, fs) {
  const file = path.resolve(root, row.path);
  const info = fs.lstatSync(file, { throwIfNoEntry: false });
  if (!info) fail(`${row.name}: the staged file ${row.path} is missing`, { name: row.name });
  if (info.isSymbolicLink() || !info.isFile()) fail(`${row.name}: ${row.path} is not a regular file`, { name: row.name });
  const actual = sha256Of(fs, file);
  if (actual !== row.sha256) fail(`${row.name}: ${row.path} has sha256 ${actual}, the receipt row says ${row.sha256}; the staged file changed after it was staged`, { name: row.name, expected: row.sha256, actual });
  return file;
}

export function readVerifiedStageReceipt({ root, receiptPath = DEFAULT_STAGE_RECEIPT, target, profile, mode, names, kind, fs = nodeFs } = {}) {
  if (typeof root !== "string" || !path.isAbsolute(root)) fail("root must be the absolute app root");
  if (!Array.isArray(names) || !names.length || names.some((name) => typeof name !== "string" || !name)) fail("names must be a non-empty list of receipt row names");
  const { receipt } = loadReceipt({ root, receiptPath, target, fs });
  if (mode !== undefined) {
    if (receipt.mode === undefined) fail(`the receipt records no build mode, so it cannot be accepted for a ${mode} build; stage again`);
    if (receipt.mode !== mode) fail(`the receipt was staged in ${receipt.mode} mode, not ${mode}`, { expected: mode, actual: receipt.mode });
  }
  const out = {};
  for (const name of names) {
    const rows = mainRows(receipt).filter((row) => row.name === name && (kind === undefined || row.kind === kind));
    if (rows.length !== 1) fail(`the receipt has ${rows.length} row(s) named ${name}${kind ? ` of kind ${kind}` : ""}, expected exactly one`, { name });
    const [row] = rows;
    if (profile !== undefined && row.profile !== profile) fail(`${name} was staged with Cargo profile ${row.profile === undefined ? "(none recorded)" : row.profile}, not ${profile}`, { name, expected: profile, actual: row.profile });
    out[name] = { path: verifyRow(root, row, fs), sha256: row.sha256 };
  }
  return out;
}

/** Validate `sign.stageReceipt`; returns { receipt, bind } (bind: array of [file, rowName]). */
export function validateStageReceiptConfig(value, label = "sign.stageReceipt") {
  if (value === null || typeof value !== "object" || Array.isArray(value)) fail(`${label} must be { receipt, bind? }`);
  const unknown = Object.keys(value).filter((key) => !["receipt", "bind"].includes(key));
  if (unknown.length) fail(`${label} has unknown key(s): ${unknown.join(", ")} (allowed: receipt, bind)`);
  const receipt = value.receipt === undefined ? DEFAULT_STAGE_RECEIPT : value.receipt;
  contained(path.sep === "\\" ? "C:\\app" : "/app", receipt, `${label}.receipt`);
  const bind = value.bind === undefined ? {} : value.bind;
  if (bind === null || typeof bind !== "object" || Array.isArray(bind)) fail(`${label}.bind must map payload file paths to receipt row names`);
  const pairs = Object.entries(bind).map(([file, name]) => {
    contained(path.sep === "\\" ? "C:\\app" : "/app", file, `${label}.bind key`);
    if (typeof name !== "string" || !name) fail(`${label}.bind[${JSON.stringify(file)}] must be a receipt row name`);
    return [file, name];
  });
  return { receipt, bind: pairs };
}

/** The release-time guard. Throws StageReceiptError naming every problem; returns { rows, bound } when it passes. */
export function assertStageReceiptGuard({ root, config, mode = "release", target, fs = nodeFs } = {}) {
  const { receipt: receiptPath, bind } = validateStageReceiptConfig(config);
  const { receipt } = loadReceipt({ root, receiptPath, target, fs });
  const problems = [];
  if (mode === "release") {
    if (receipt.mode !== "release") problems.push(receipt.mode === undefined ? "the receipt records no build mode (a release needs a receipt staged in release mode)" : `the receipt was staged in ${receipt.mode} mode; a release build refuses a ${receipt.mode} receipt`);
    for (const row of mainRows(receipt)) if (row.profile !== undefined && row.profile !== "release") problems.push(`${row.name} was staged with Cargo profile ${row.profile}; a release build needs release`);
  }
  const rows = mainRows(receipt);
  for (const row of rows) { try { verifyRow(root, row, fs); } catch (error) { problems.push(error.message); } }
  const bound = [];
  for (const [file, name] of bind) {
    const row = rows.find((item) => item.name === name);
    if (!row) { problems.push(`${file} is bound to ${name}, which has no row in the receipt`); continue; }
    const full = contained(root, file, "bound file");
    const info = fs.lstatSync(full, { throwIfNoEntry: false });
    if (!info || info.isSymbolicLink() || !info.isFile()) { problems.push(`${file} (bound to ${name}) is missing or not a regular file`); continue; }
    const actual = sha256Of(fs, full);
    if (actual !== row.sha256) problems.push(`${file} (bound to ${name}) has sha256 ${actual}, the receipt says ${row.sha256}: the hand-assembled copy is not the staged file`);
    else bound.push({ file, name, sha256: actual });
  }
  if (problems.length) fail(`the stage receipt guard refused signing:\n${problems.map((line) => `- ${line}`).join("\n")}`, { problems });
  return { rows: rows.length, bound };
}
