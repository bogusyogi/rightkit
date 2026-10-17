#!/usr/bin/env node
// Chrome for Testing pins (#267): list them, or write recorded digests into the sources.
//
//   node scripts/chrome-pins.mjs                       print the pins as JSON [{ kind, platform, version, url }]
//   node scripts/chrome-pins.mjs --tsv                 the same, one tab-separated line per pin (kind platform version url)
//   node scripts/chrome-pins.mjs --apply <file.json>   write sha256 digests from the chrome-pins workflow's
//                                                      chrome-pins.json into both pin sources
//
// The two pin sources are:
//   crates/rightkit-browser/src/install.rs   cft_chrome!("<version>", "<platform>"[, "<sha256>"]) and the
//                                            headless-shell PinnedDownload with `sha256: Sha256Pin::Todo|Pinned("..")`
//   packages/qa-chrome/src/install.ts        the version, the platform list, and the CHROME_SHA256 table between
//                                            the `chrome-pins:begin` / `chrome-pins:end` markers
// A missing digest is `Sha256Pin::Todo` in Rust and a missing CHROME_SHA256 entry in TypeScript.
// Extraction fails when the two sources disagree on a version or URL. Byte sizes in chrome-pins.json are
// checked for shape only: neither source has a field to hold one.

import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const RUST_PATH = path.join(ROOT, 'crates/rightkit-browser/src/install.rs');
const TS_PATH = path.join(ROOT, 'packages/qa-chrome/src/install.ts');
const BASE = 'https://storage.googleapis.com/chrome-for-testing-public/';
const VERSION_RE = /^\d+(\.\d+)+$/;
const SHA_RE = /^[0-9a-f]{64}$/;

const fail = (message) => {
  console.error(`chrome-pins: ${message}`);
  process.exit(1);
};

const urlFor = (version, platform, name) => `${BASE}${version}/${platform}/${name}-${platform}.zip`;
const keyOf = (p) => `${p.kind}/${p.platform}`;

/** Pins from the Rust table. */
function parseRust(text) {
  if (!text.includes(`"${BASE}"`)) fail('install.rs: cft_url! no longer uses the expected Chrome for Testing base URL');
  const table = text.match(/PINNED_CHROME_FOR_TESTING:\s*PinnedChrome\s*=\s*PinnedChrome\s*\{[\s\S]*?\n\};/);
  if (!table) fail('install.rs: PINNED_CHROME_FOR_TESTING not found');
  const body = table[0];
  const tableVersion = body.match(/\n\s*version:\s*"([^"]+)"/)?.[1];
  if (!tableVersion || !VERSION_RE.test(tableVersion)) fail('install.rs: table version missing or malformed');
  const pins = [];
  for (const m of body.matchAll(/cft_chrome!\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*(?:,\s*"([^"]*)"\s*)?\)/g)) {
    pins.push({ kind: 'chrome', platform: m[2], version: m[1], url: urlFor(m[1], m[2], 'chrome'), sha256: m[3] ?? null });
  }
  for (const m of body.matchAll(
    /cft_url!\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*"chrome-headless-shell"\s*\)\s*,\s*sha256:\s*Sha256Pin::(?:Todo|Pinned\("([^"]*)"\))/g,
  )) {
    pins.push({ kind: 'headless_shell', platform: m[2], version: m[1], url: urlFor(m[1], m[2], 'chrome-headless-shell'), sha256: m[3] ?? null });
  }
  if (pins.length === 0) fail('install.rs: no pins found');
  for (const p of pins) {
    if (p.version !== tableVersion) fail(`install.rs: ${keyOf(p)} is ${p.version} but the table version is ${tableVersion}`);
  }
  return { version: tableVersion, pins };
}

/** Pins from the TypeScript table (Chrome archives only). */
function parseTs(text) {
  if (!text.includes(`${BASE}\${version}/\${platform}/chrome-\${platform}.zip`)) {
    fail('install.ts: the cft() URL template no longer matches the expected Chrome for Testing URL');
  }
  const table = text.match(/PINNED_CHROME_FOR_TESTING:\s*PinnedChrome\s*=\s*\{[\s\S]*?\n\};/);
  if (!table) fail('install.ts: PINNED_CHROME_FOR_TESTING not found');
  const body = table[0];
  const tableVersion = body.match(/\n\s*version:\s*'([^']+)'/)?.[1];
  if (!tableVersion || !VERSION_RE.test(tableVersion)) fail('install.ts: table version missing or malformed');
  const list = body.match(/\[([^\]]+)\]\.map\(\(p\)\s*=>\s*cft\('([^']+)',\s*p\)\)/);
  if (!list) fail('install.ts: the platform list passed to cft() not found');
  if (list[2] !== tableVersion) fail(`install.ts: cft() version ${list[2]} differs from the table version ${tableVersion}`);
  const platforms = [...list[1].matchAll(/'([^']+)'/g)].map((m) => m[1]);
  if (platforms.length === 0) fail('install.ts: empty platform list');
  const sha = tsDigests(text);
  const pins = platforms.map((platform) => ({
    kind: 'chrome',
    platform,
    version: tableVersion,
    url: urlFor(tableVersion, platform, 'chrome'),
    sha256: sha.get(platform) ?? null,
  }));
  return { version: tableVersion, pins, platforms };
}

const TS_BLOCK = /(\/\/ chrome-pins:begin\r?\n)([\s\S]*?)(\s*\/\/ chrome-pins:end)/;

function tsDigests(text) {
  const block = text.match(TS_BLOCK);
  if (!block) fail('install.ts: chrome-pins:begin / chrome-pins:end markers not found');
  const map = new Map();
  for (const m of block[2].matchAll(/^\s*'([^']+)':\s*'([^']*)',?\s*$/gm)) map.set(m[1], m[2]);
  return map;
}

function extract() {
  const rust = parseRust(readFileSync(RUST_PATH, 'utf8'));
  const ts = parseTs(readFileSync(TS_PATH, 'utf8'));
  if (rust.version !== ts.version) {
    fail(`version differs: install.rs ${rust.version} vs install.ts ${ts.version}`);
  }
  const rustByKey = new Map(rust.pins.map((p) => [keyOf(p), p]));
  if (rustByKey.size !== rust.pins.length) fail('install.rs: a platform/kind is pinned twice');
  const tsKeys = new Set();
  for (const t of ts.pins) {
    const k = keyOf(t);
    if (tsKeys.has(k)) fail(`install.ts: ${k} is pinned twice`);
    tsKeys.add(k);
    const r = rustByKey.get(k);
    if (!r) fail(`${k} is pinned in install.ts but not in install.rs`);
    if (r.version !== t.version) fail(`${k}: version differs: install.rs ${r.version} vs install.ts ${t.version}`);
    if (r.url !== t.url) fail(`${k}: URL differs:\n  install.rs ${r.url}\n  install.ts ${t.url}`);
  }
  for (const r of rust.pins) {
    if (r.kind === 'chrome' && !tsKeys.has(keyOf(r))) fail(`${keyOf(r)} is pinned in install.rs but not in install.ts`);
  }
  for (const r of rust.pins) {
    if (!r.url.startsWith(BASE)) fail(`${keyOf(r)}: URL is not under ${BASE}`);
  }
  return { rust, ts, pins: rust.pins.map(({ kind, platform, version, url }) => ({ kind, platform, version, url })) };
}

function apply(file) {
  const { pins } = extract();
  let recorded;
  try {
    recorded = JSON.parse(readFileSync(file, 'utf8'));
  } catch (e) {
    fail(`cannot read ${file}: ${e.message}`);
  }
  if (!Array.isArray(recorded)) fail(`${file}: expected a JSON array`);
  const byKey = new Map();
  for (const e of recorded) {
    if (typeof e !== 'object' || e === null) fail(`${file}: entry is not an object`);
    const k = keyOf(e);
    if (byKey.has(k)) fail(`${file}: ${k} listed twice`);
    byKey.set(k, e);
  }
  const digests = new Map();
  for (const p of pins) {
    const k = keyOf(p);
    const e = byKey.get(k);
    if (!e) fail(`${file}: no entry for ${k}`);
    if (e.version !== p.version) fail(`${file}: ${k} is version ${e.version}, the pin is ${p.version} (re-run the workflow)`);
    if (e.url !== p.url) fail(`${file}: ${k} URL differs from the pin (re-run the workflow)`);
    if (typeof e.sha256 !== 'string' || !SHA_RE.test(e.sha256)) fail(`${file}: ${k} sha256 is not 64 lowercase hex characters`);
    if (!Number.isInteger(e.size) || e.size <= 0) fail(`${file}: ${k} size is not a positive integer`);
    digests.set(k, e.sha256);
  }

  // Rust
  let rs = readFileSync(RUST_PATH, 'utf8');
  const tableStart = rs.search(/PINNED_CHROME_FOR_TESTING:\s*PinnedChrome/);
  const head = rs.slice(0, tableStart);
  let tail = rs.slice(tableStart);
  tail = tail.replace(
    /cft_chrome!\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*(?:,\s*"[^"]*"\s*)?\)/g,
    (_m, v, p) => `cft_chrome!("${v}", "${p}", "${digests.get(`chrome/${p}`)}")`,
  );
  tail = tail.replace(
    /(cft_url!\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*"chrome-headless-shell"\s*\)\s*,\s*sha256:\s*)Sha256Pin::(?:Todo|Pinned\("[^"]*"\))/g,
    (_m, lead, _v, p) => `${lead}Sha256Pin::Pinned("${digests.get(`headless_shell/${p}`)}")`,
  );
  writeFileSync(RUST_PATH, head + tail);

  // TypeScript
  let ts = readFileSync(TS_PATH, 'utf8');
  const { platforms } = parseTs(ts);
  const lines = platforms.map((p) => `  '${p}': '${digests.get(`chrome/${p}`)}',`).join('\n');
  ts = ts.replace(TS_BLOCK, (_m, begin, _body, end) => `${begin}${lines}${end}`);
  writeFileSync(TS_PATH, ts);

  // Prove the result still parses and agrees.
  const after = extract();
  const rustAfter = parseRust(readFileSync(RUST_PATH, 'utf8'));
  const tsAfter = parseTs(readFileSync(TS_PATH, 'utf8'));
  for (const p of [...rustAfter.pins, ...tsAfter.pins]) {
    if (p.sha256 !== digests.get(keyOf(p))) fail(`${keyOf(p)}: digest did not land in the source`);
  }
  console.log(`chrome-pins: recorded ${digests.size} digest(s) for ${after.rust.version} in install.rs and install.ts`);
}

const args = process.argv.slice(2);
if (args[0] === '--apply') {
  if (!args[1]) fail('--apply needs the chrome-pins.json path');
  apply(args[1]);
} else if (args[0] === '--tsv') {
  for (const p of extract().pins) console.log([p.kind, p.platform, p.version, p.url].join('\t'));
} else if (args.length === 0) {
  console.log(JSON.stringify(extract().pins, null, 2));
} else {
  fail(`unknown arguments: ${args.join(' ')}`);
}
