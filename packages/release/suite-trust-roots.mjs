// The suite's trust roots have one owner, contracts/right-suite-api.v1.json (schema right-suite-api.v1). These preflight rules find
// what an app does instead of taking them from it: a pasted copy of the licence public key (any spelling), an updater key in a
// Tauri config that the contract does not list, and the engine reading the owner's record store itself. Pure functions over
// (root, files), each returning [{ file, subject, problem, fix }]; packages/release/app-check.mjs registers them (lane C3, so an app's `rightkit-gate check` runs
// them too) and an app's existing findings are ratcheted like G13-G21.
import fs from "node:fs";
import path from "node:path";
import { RIGHT_SUITE_LICENSE_PUBLIC_KEY_HEX, RIGHT_SUITE_UPDATER_PUBLIC_KEYS } from "./suite-api.generated.mjs";

const literal = (text) => text.replace(/[\\^$.*+?()[\]{}|/]/g, "\\$&");

// Files the rules do not read: documentation and dependency trees, and test code, which pastes keys on purpose to prove them.
// Segments of the test directories are matched without case: Swift packages call theirs Tests.
const SKIPPED_TREE = /(^|\/)(docs|node_modules|target|\.git)\//;
const TEST_SEGMENT = /(^|\/)(tests?|__tests__|test-vectors|fixtures)(\/|$)/i;
const TEST_NAME = /\.(?:test|spec)\.|_tests?\./;
const scanned = (file) => !SKIPPED_TREE.test(file) && !TEST_SEGMENT.test(file) && !TEST_NAME.test(path.posix.basename(file)) && !/\.md$/i.test(file);

// A tauri*.conf.json carries the updater key by design (suiteUpdaterKey checks that copy), so the pasted-key rule leaves it alone.
const TAURI_CONF = /(^|\/)tauri[^/]*\.conf\.json$/;

// What the contract's owner generates from it: RightKit holds the key by definition, apps do not. RightKit is recognised by its
// generator, which no app carries.
const OWNER_GENERATOR = "scripts/gen-suite-api.mjs";
const OWNER_FILES = new Set([
  "contracts/right-suite-api.v1.json",
  "crates/rightkit-license/src/suite_api_generated.rs",
  "crates/rightkit-license/test-vectors/right-suite-api.v1.json",
  "packages/app-update/src/suite-api.generated.ts",
  "packages/release/suite-api.generated.mjs",
  "swift/RightKitSwift/Sources/RightKitLicense/RightSuiteAPI.generated.swift",
]);

/** The text of a tracked file, or null when it cannot be read or is binary (a NUL byte: images, archives, UTF-16). */
function textOf(root, file) {
  let bytes;
  try { bytes = fs.readFileSync(path.join(root, file)); } catch { return null; }
  return bytes.includes(0) ? null : bytes.toString("utf8");
}

/** The distinct 1-based lines on which `pattern` (global) starts a match, ascending. */
function matchLines(text, pattern) {
  const lines = [];
  let line = 1;
  let at = 0;
  for (const m of text.matchAll(pattern)) {
    for (; at < m.index; at += 1) if (text.charCodeAt(at) === 10) line += 1;
    if (lines.at(-1) !== line) lines.push(line);
  }
  return lines;
}

const listed = (lines) => `${lines.slice(0, 8).join(",")}${lines.length > 8 ? ",…" : ""}`;
const lineAt = (text, offset) => (offset < 0 ? 1 : text.slice(0, offset).split("\n").length);

// An element of a pasted byte array may carry Rust's integer suffix.
const INT_SUFFIX = "(?:_?u8)?";
const SEPARATOR = "\\s*,\\s*";

/** One pattern per way an app pastes the licence key, each global: [{ subject, what, pattern }], derived from the contract's hex so
 * a rotation changes what is found without touching this file. The 64-digit hex and the base64 forms are matched inside longer
 * text on purpose: a PEM or a concatenation that contains the key still pastes it, and no other text contains it by chance. */
export function licenceKeyPatterns(hex = RIGHT_SUITE_LICENSE_PUBLIC_KEY_HEX) {
  if (!/^[0-9a-f]{64}$/i.test(hex)) throw new Error(`the suite licence public key is not 64 hex digits: ${JSON.stringify(hex)}`);
  const key = Buffer.from(hex, "hex");
  const bytes = [...key];
  const array = (element) => bytes.map(element).join(SEPARATOR);
  const decimal = array((b) => `${b}${INT_SUFFIX}`);
  // 0x7 and 0x07 are the same byte.
  const prefixed = array((b) => `0x${b < 16 ? "0?" : ""}${b.toString(16)}${INT_SUFFIX}`);
  // The two base64 alphabets differ only at + / and - _, so a key whose encoding has none of them reads the same in both. The
  // unpadded text then counts as base64url and only the padded text as base64, which keeps one paste at one finding.
  const url = key.toString("base64url");
  const standard = key.toString("base64").replace(/=+$/, "");
  const same = url === standard;
  return [
    { subject: "licence-key:bytes", what: "as a byte array", pattern: new RegExp(`(?<![\\w.])(?:${decimal}|${prefixed})(?![\\w.])`, "gi") },
    { subject: "licence-key:hex", what: "as 64 hex digits", pattern: new RegExp(hex, "gi") },
    { subject: "licence-key:base64url", what: "as unpadded base64url", pattern: new RegExp(`${literal(url)}${same ? "(?!=)" : ""}`, "g") },
    { subject: "licence-key:base64", what: "as base64", pattern: new RegExp(`${literal(standard)}${same ? "=" : "=?"}`, "g") },
  ];
}

/** The pattern for the text of any contract updater key (the base64 a tauri.conf.json carries), or null with no keys. */
export function updaterKeyPattern(keys = RIGHT_SUITE_UPDATER_PUBLIC_KEYS) {
  const texts = keys.map((k) => k.publicKey);
  // An empty alternative would match every file.
  if (texts.some((t) => typeof t !== "string" || t === "")) throw new Error("a suite updater public key has no text");
  return texts.length ? new RegExp(texts.map(literal).join("|"), "g") : null;
}

const KEY_SOURCES = "rightkit_license::suite_api::LICENSE_PUBLIC_KEY (Rust), the suite-api export of @rightkit/app-update (TypeScript) or RightSuiteAPI (Swift)";

/** The licence public key in any spelling, or the updater key text outside a tauri*.conf.json, in a tracked non-test file. One
 * finding per file and spelling; the line numbers are in the problem text only, so an edit above does not change the identity. */
export function suiteKeyLiteral(root, files) {
  const updater = updaterKeyPattern();
  const forms = [
    ...licenceKeyPatterns().map((form) => ({ ...form, what: `carries the suite licence public key ${form.what}`, fix: `take the key from ${KEY_SOURCES}; never paste a copy` })),
    ...(updater ? [{ subject: "updater-key", what: "carries the suite updater public key text", pattern: updater, skipsTauriConf: true, fix: `plugins.updater.pubkey of a tauri.conf.json is the one committed copy (Tauri's plugin config requires it); anywhere else take the key from ${KEY_SOURCES}` }] : []),
  ];
  const owner = files.includes(OWNER_GENERATOR);
  const out = [];
  for (const file of files) {
    if (!scanned(file) || (owner && OWNER_FILES.has(file))) continue;
    const text = textOf(root, file);
    if (text === null) continue;
    for (const form of forms) {
      if (form.skipsTauriConf && TAURI_CONF.test(file)) continue;
      const lines = matchLines(text, form.pattern);
      if (lines.length) out.push({ file, subject: form.subject, problem: `${file}:${listed(lines)} ${form.what}`, fix: form.fix });
    }
  }
  return out;
}

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/** Every tracked tauri*.conf.json whose plugins.updater block does not carry a copy of a contract key: a missing key, a placeholder,
 * another key, or a file that is not JSON. A file with no plugins.updater block (a platform override) has nothing to check. */
export function suiteUpdaterKey(root, files) {
  const accepted = new Set(RIGHT_SUITE_UPDATER_PUBLIC_KEYS.map((k) => k.publicKey));
  const fix = "set plugins.updater.pubkey to a copy of a publicKey in contracts/right-suite-api.v1.json updaterPublicKeys; the conf holds a copy only because Tauri's plugin config requires one, and the app takes the key from the contract everywhere else";
  const out = [];
  for (const file of files) {
    if (!TAURI_CONF.test(file) || !scanned(file)) continue;
    const text = textOf(root, file);
    if (text === null) continue;
    let conf;
    // A byte-order mark is how some Windows editors save JSON; Tauri reads it, so it is not a defect here.
    try { conf = JSON.parse(text.replace(/^﻿/, "")); } catch { out.push({ file, subject: "unparseable", problem: `${file} is not valid JSON, so its plugins.updater.pubkey cannot be checked`, fix }); continue; }
    const updater = conf?.plugins?.updater;
    if (!isObject(updater)) continue;
    const block = text.search(/"updater"\s*:/);
    const { pubkey } = updater;
    let why;
    let offset = block;
    if (pubkey === undefined) why = "has no plugins.updater.pubkey";
    else {
      offset = text.indexOf('"pubkey"', Math.max(block, 0));
      if (typeof pubkey !== "string") why = "has a plugins.updater.pubkey that is not a string";
      else if (!accepted.has(pubkey)) why = `has plugins.updater.pubkey "${pubkey.slice(0, 32)}${pubkey.length > 32 ? "…" : ""}", which is not a publicKey of the suite contract`;
    }
    if (why) out.push({ file, subject: "plugins.updater.pubkey", problem: `${file}:${lineAt(text, offset)} ${why}`, fix });
  }
  return out;
}

// The owner binary hands the engine a snapshot of the licence record (rightkit_credentials::handoff); an engine that opens the record
// store itself reads a keychain item across binaries. Assembled so this file does not itself carry the call it refuses.
const RECORD_STORE_READER = "from_record_store";
const RECORD_STORE_CALL = new RegExp(literal(`${RECORD_STORE_READER}(`), "g");
// The crate that defines the reader, and the one place that may call it.
const RECORD_STORE_OWNER = "crates/rightkit-license/";

/** A tracked non-test file outside the licence crate that calls the engine's record-store reader. */
export function engineRecordStore(root, files) {
  const out = [];
  for (const file of files) {
    if (!scanned(file) || file.startsWith(RECORD_STORE_OWNER)) continue;
    const text = textOf(root, file);
    if (text === null || !text.includes(`${RECORD_STORE_READER}(`)) continue;
    out.push({ file, subject: RECORD_STORE_READER, problem: `${file}:${listed(matchLines(text, RECORD_STORE_CALL))} calls ${RECORD_STORE_READER}: the engine reads the owner's record store itself`, fix: 'have the owner binary hand the engine a snapshot through rightkit_credentials::handoff instead (rightkit-credentials README "Helpers, sidecars and CLIs")' });
  }
  return out;
}
