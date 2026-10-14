// Release-blocking scan: a shipped binary must not contain any test-automation or debugging surface.
//
// Owner requirement: NO test-automation surface of any kind ships; dev builds may contain them. This module is
// the binary layer of that rule. It scans Mach-O, PE and ELF files (and the native files inside ZIP-based
// containers such as MSIX/APPX) for four independent rules, and is called for release and update builds
// (signing set, staged payloads, hardening targets, sealed files). The development lane (`development.mjs`)
// never calls it: dev builds are the QA builds. The source-level layer is `qa-build-features.mjs`.
//
//   qa-control-server  `rightkit-qa-driver` feature `qa-control`: every binary with the embedded server carries
//                      QA_CONTROL_MARKER (a `#[used]` static in `embedded/mod.rs`, also read by `Control::build`).
//   wdio-webdriver     `tauri-plugin-wdio-webdriver` (HeardRight/MailRight/ViewRight `qa-native`): needs 2 of 3
//                      independent signatures. WEAKER than the marker: the plugin source was not on disk when
//                      the signatures were chosen, they come from Rust path/module/permission naming rules.
//   shell-dev-inspect  `rightkit-shell` feature `dev-inspect` (developer inspection of an app's own webviews): every
//                      binary built with it carries DEV_INSPECT_MARKER. Refused in every tier this module scans.
//   remote-debugging   `--remote-debugging-port` / `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` baked into a binary.
//   tauri-devtools     Tauri's `devtools` feature (or a debug build): the toggle-devtools init script. Accepted only for an
//                      allow-listed app (devtools-policy.json) whose binary carries CONTENT_ONLY_MARKER.
import { closeSync, lstatSync, openSync, readFileSync, readSync, readdirSync } from "node:fs";
import path from "node:path";
import { inflateRawSync } from "node:zlib";

// Keep identical to `QA_CONTROL_MARKER` in crates/rightkit-qa-driver/src/embedded/mod.rs.
export const QA_CONTROL_MARKER = "RIGHTKIT-QA-CONTROL-SERVER-PRESENT";
// Keep identical to `DEV_INSPECT_MARKER` in crates/rightkit-shell/src/devtools.rs (a `#[used]` static compiled only
// with rightkit-shell's `dev-inspect` feature and read by the process guard). No allow-list lifts this rule.
export const DEV_INSPECT_MARKER = "RIGHTKIT-SHELL-DEV-INSPECT-COMPILED";
const MARKER = Buffer.from(QA_CONTROL_MARKER, "latin1");
const CHUNK = 4 * 1024 * 1024;

// Each signature is matched as Latin-1/UTF-8 bytes and as UTF-16LE (Windows wide strings). `min` signatures of a
// rule must be present for it to fire; a signature counts once however often or in whichever encoding it occurs.
export const QA_RULES = Object.freeze([
  { id: "qa-control-server", min: 1, label: "RightKit QA control server (rightkit-qa-driver qa-control)",
    signatures: [{ id: "marker", text: QA_CONTROL_MARKER }] },
  { id: "shell-dev-inspect", min: 1, marker: DEV_INSPECT_MARKER, label: "rightkit-shell developer inspection (rightkit-shell dev-inspect)",
    signatures: [{ id: "marker", text: DEV_INSPECT_MARKER }] },
  { id: "wdio-webdriver", min: 2, label: "tauri-plugin-wdio-webdriver test driver (app qa-native/dev-native)",
    signatures: [
      { id: "crate-path", text: "tauri-plugin-wdio-webdriver" },
      { id: "module-path", text: "tauri_plugin_wdio_webdriver" },
      { id: "permission", text: "wdio-webdriver:default" },
    ] },
  { id: "remote-debugging", min: 1, label: "remote-debugging enablement baked into the binary",
    signatures: [
      { id: "chromium-flag", text: "--remote-debugging-port" },
      { id: "webview2-env", text: "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS" },
    ] },
  { id: "tauri-devtools", min: 1, label: "Tauri devtools (feature `devtools` or a debug build)",
    signatures: [
      { id: "invoke", text: "plugin:webview|internal_toggle_devtools" },
      { id: "hotkey-script", text: "toggleDevtoolsHotkey" },
    ] },
]);

// Keep identical to `CONTENT_ONLY_MARKER` in crates/rightkit-shell/src/devtools.rs. A binary carries it when it was
// built with rightkit-shell's `content-devtools` feature. It is not a finding; it is the evidence that lets an
// allow-listed app (devtools-policy.json) ship Tauri's devtools code for ViewRight-style content inspection.
export const CONTENT_ONLY_MARKER = "RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED";
const CONTENT_ONLY_KEY = "content-only-marker/marker";

const NEEDLES = [...QA_RULES.flatMap((rule) => rule.signatures.map((signature) => ({
  rule: rule.id, signature: signature.id,
  buffers: [Buffer.from(signature.text, "latin1"), Buffer.from(signature.text, "utf16le")],
}))), {
  rule: "content-only-marker", signature: "marker",
  buffers: [Buffer.from(CONTENT_ONLY_MARKER, "latin1"), Buffer.from(CONTENT_ONLY_MARKER, "utf16le")],
}];
const MAX_NEEDLE = Math.max(...NEEDLES.flatMap((n) => n.buffers.map((b) => b.length)));

// Incremental matcher: feed chunks in order; keeps an overlap so a signature split across chunks is found.
export class RuleScanner {
  constructor() { this.tail = Buffer.alloc(0); this.hits = new Map(); }
  push(chunk) {
    const window = this.tail.length ? Buffer.concat([this.tail, chunk]) : chunk;
    for (const needle of NEEDLES) {
      const key = `${needle.rule}/${needle.signature}`;
      if (this.hits.has(key)) continue;
      if (needle.buffers.some((b) => window.includes(b))) this.hits.set(key, needle);
    }
    this.tail = Buffer.from(window.subarray(Math.max(0, window.length - (MAX_NEEDLE - 1))));
  }
  // `allowContentOnlyDevtools`: the app is allow-listed for rightkit-shell's ContentOnly DevTools policy. Then Tauri's
  // devtools signatures are accepted only in a binary that also carries CONTENT_ONLY_MARKER; one without it is still
  // a finding (devtools compiled in by some other route).
  findings({ allowContentOnlyDevtools = false } = {}) {
    const out = [];
    for (const rule of QA_RULES) {
      if (rule.id === "tauri-devtools" && allowContentOnlyDevtools && this.hits.has(CONTENT_ONLY_KEY)) continue;
      const matched = rule.signatures.filter((s) => this.hits.has(`${rule.id}/${s.id}`));
      if (matched.length < rule.min) continue;
      out.push({ rule: rule.id, reason: rule.id === "qa-control-server" || rule.marker
        ? `contains ${rule.marker ?? QA_CONTROL_MARKER}`
        : `${rule.label}: ${matched.length} of ${rule.signatures.length} signatures (${matched.map((s) => s.text).join(", ")})` });
    }
    return out;
  }
}

// Mach-O thin (both endians, 32/64-bit), fat/universal (0xCAFEBABE, 0xCAFEBABF), PE ("MZ"), ELF.
const MAGICS = [
  [0xfe, 0xed, 0xfa, 0xce], [0xfe, 0xed, 0xfa, 0xcf], [0xce, 0xfa, 0xed, 0xfe], [0xcf, 0xfa, 0xed, 0xfe],
  [0xca, 0xfe, 0xba, 0xbe], [0xca, 0xfe, 0xba, 0xbf], [0x7f, 0x45, 0x4c, 0x46],
].map((bytes) => Buffer.from(bytes));
const MZ = Buffer.from([0x4d, 0x5a]);

export function isNativeBinaryHeader(head) {
  if (!Buffer.isBuffer(head)) return false;
  if (head.length >= 2 && head.subarray(0, 2).equals(MZ)) return true;
  return MAGICS.some((magic) => head.length >= 4 && head.subarray(0, 4).equals(magic));
}

export function isZipHeader(head) {
  return Buffer.isBuffer(head) && head.length >= 4 && head[0] === 0x50 && head[1] === 0x4b
    && ((head[2] === 0x03 && head[3] === 0x04) || (head[2] === 0x05 && head[3] === 0x06));
}

export function bufferHasQaControlMarker(buffer) {
  return Buffer.from(buffer).includes(MARKER);
}

// All rules over one in-memory buffer (a sealed file). Looks inside ZIP containers too.
export function bufferQaFindings(buffer, options = {}) {
  const bytes = Buffer.from(buffer);
  if (isZipHeader(bytes.subarray(0, 4))) {
    const findings = []; const stats = newStats();
    scanZip(bufferSource(bytes), "<buffer>", { findings, stats, allowContentOnlyDevtools: options.allowContentOnlyDevtools === true, limits: limitsOf(options), budget: { left: limitsOf(options).totalInflated } }, 0);
    return findings;
  }
  const scanner = new RuleScanner();
  for (let at = 0; at < bytes.length; at += CHUNK) scanner.push(bytes.subarray(at, Math.min(bytes.length, at + CHUNK)));
  return scanner.findings({ allowContentOnlyDevtools: options.allowContentOnlyDevtools === true });
}

// Streams the file in chunks with an overlap, so a marker split across chunks is found. Marker rule only.
export function fileHasQaControlMarker(file) {
  const fd = openSync(file, "r");
  try {
    const buffer = Buffer.alloc(CHUNK);
    let tail = Buffer.alloc(0);
    for (;;) {
      const read = readSync(fd, buffer, 0, CHUNK, null);
      if (read === 0) return false;
      const window = Buffer.concat([tail, buffer.subarray(0, read)]);
      if (window.includes(MARKER)) return true;
      tail = Buffer.from(window.subarray(Math.max(0, window.length - (MARKER.length - 1))));
    }
  } finally {
    closeSync(fd);
  }
}

function readHead(file) {
  const fd = openSync(file, "r");
  try {
    const head = Buffer.alloc(4);
    const read = readSync(fd, head, 0, 4, 0);
    return head.subarray(0, read);
  } finally {
    closeSync(fd);
  }
}

// ---- byte sources and the ZIP reader ------------------------------------------------------------------------
const DEFAULT_LIMITS = Object.freeze({
  maxEntries: 100_000,
  maxCentralDirectory: 256 * 1024 * 1024,
  maxEntryBytes: 1024 * 1024 * 1024, // largest single entry we will inflate in memory
  totalInflated: 8 * 1024 * 1024 * 1024, // zip-bomb budget per scanned container
  maxDepth: 3,
});
const limitsOf = (options) => ({ ...DEFAULT_LIMITS, ...(options.limits ?? {}) });
const newStats = () => ({ binaries: 0, containers: 0, entries: 0, opaque: [] });

function bufferSource(buffer) {
  return { size: buffer.length, read: (position, length) => buffer.subarray(position, position + length) };
}
function fdSource(fd, size) {
  return { size, read: (position, length) => {
    const out = Buffer.alloc(length); let got = 0;
    while (got < length) {
      const n = readSync(fd, out, got, length - got, position + got);
      if (n === 0) break;
      got += n;
    }
    return out.subarray(0, got);
  } };
}
const sliceSource = (source, start, length) => ({ size: length, read: (position, n) => source.read(start + position, Math.min(n, length - position)) });

function u64(buffer, offset) {
  const value = buffer.readBigUInt64LE(offset);
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("zip64 value out of range");
  return Number(value);
}

function readEndOfCentralDirectory(source) {
  const tailLength = Math.min(source.size, 22 + 65535);
  const tail = source.read(source.size - tailLength, tailLength);
  for (let i = tail.length - 22; i >= 0; i -= 1) {
    if (tail.readUInt32LE(i) !== 0x06054b50) continue;
    if (i + 22 + tail.readUInt16LE(i + 20) > tail.length) continue;
    let entries = tail.readUInt16LE(i + 10); let size = tail.readUInt32LE(i + 12); let offset = tail.readUInt32LE(i + 16);
    if (entries === 0xffff || size === 0xffffffff || offset === 0xffffffff) {
      const eocdPosition = source.size - tailLength + i;
      if (eocdPosition < 20) throw new Error("zip64 locator missing");
      const locator = source.read(eocdPosition - 20, 20);
      if (locator.length < 20 || locator.readUInt32LE(0) !== 0x07064b50) throw new Error("zip64 locator missing");
      const record = source.read(u64(locator, 8), 56);
      if (record.length < 56 || record.readUInt32LE(0) !== 0x06064b50) throw new Error("zip64 end record missing");
      entries = u64(record, 32); size = u64(record, 40); offset = u64(record, 48);
    }
    return { entries, size, offset };
  }
  throw new Error("end of central directory not found");
}

function* zipEntries(source, limits) {
  const eocd = readEndOfCentralDirectory(source);
  if (eocd.entries > limits.maxEntries) throw new Error(`more than ${limits.maxEntries} entries`);
  if (eocd.size > limits.maxCentralDirectory || eocd.offset + eocd.size > source.size) throw new Error("central directory out of bounds");
  const directory = source.read(eocd.offset, eocd.size);
  if (directory.length !== eocd.size) throw new Error("central directory truncated");
  let at = 0;
  for (let n = 0; n < eocd.entries; n += 1) {
    if (at + 46 > directory.length || directory.readUInt32LE(at) !== 0x02014b50) throw new Error("bad central directory entry");
    const flags = directory.readUInt16LE(at + 8); const method = directory.readUInt16LE(at + 10);
    let compressed = directory.readUInt32LE(at + 20); let uncompressed = directory.readUInt32LE(at + 24);
    const nameLength = directory.readUInt16LE(at + 28); const extraLength = directory.readUInt16LE(at + 30);
    const commentLength = directory.readUInt16LE(at + 32); let localOffset = directory.readUInt32LE(at + 42);
    const end = at + 46 + nameLength + extraLength + commentLength;
    if (end > directory.length) throw new Error("central directory entry out of bounds");
    const name = directory.subarray(at + 46, at + 46 + nameLength).toString("utf8");
    const extra = directory.subarray(at + 46 + nameLength, at + 46 + nameLength + extraLength);
    for (let e = 0; e + 4 <= extra.length;) {
      const id = extra.readUInt16LE(e); const size = extra.readUInt16LE(e + 2);
      if (id === 0x0001) {
        let p = e + 4;
        if (uncompressed === 0xffffffff) { uncompressed = u64(extra, p); p += 8; }
        if (compressed === 0xffffffff) { compressed = u64(extra, p); p += 8; }
        if (localOffset === 0xffffffff) { localOffset = u64(extra, p); }
      }
      e += 4 + size;
    }
    yield { name, flags, method, compressed, uncompressed, localOffset };
    at = end;
  }
}

function entryDataStart(source, entry) {
  const local = source.read(entry.localOffset, 30);
  if (local.length < 30 || local.readUInt32LE(0) !== 0x04034b50) throw new Error("bad local header");
  const start = entry.localOffset + 30 + local.readUInt16LE(26) + local.readUInt16LE(28);
  if (start + entry.compressed > source.size) throw new Error("entry data out of bounds");
  return start;
}

function pushBuffer(scanner, buffer) {
  for (let at = 0; at < buffer.length; at += CHUNK) scanner.push(buffer.subarray(at, Math.min(buffer.length, at + CHUNK)));
}

function scanZip(source, label, ctx, depth) {
  const { findings, stats, limits } = ctx;
  const bad = (file, why) => findings.push({ file, rule: "unscannable", reason: `could not scan: ${why}` });
  if (depth > limits.maxDepth) { bad(label, `containers nested deeper than ${limits.maxDepth}`); return; }
  stats.containers += 1;
  try {
    for (const entry of zipEntries(source, limits)) {
      if (entry.name.endsWith("/")) continue;
      stats.entries += 1;
      const file = `${label}!/${entry.name}`;
      try {
        if (entry.flags & 1) { bad(file, "encrypted entry"); continue; }
        const start = entryDataStart(source, entry);
        let body; // a byte source for the entry's content
        if (entry.method === 0) {
          body = sliceSource(source, start, entry.compressed);
        } else if (entry.method === 8) {
          if (entry.uncompressed > limits.maxEntryBytes || entry.compressed > limits.maxEntryBytes) { bad(file, `entry larger than ${limits.maxEntryBytes} bytes`); continue; }
          if ((ctx.budget.left -= entry.uncompressed) < 0) { bad(file, "container exceeds its inflate budget"); continue; }
          const inflated = inflateRawSync(source.read(start, entry.compressed), { maxOutputLength: Math.max(1, Math.min(limits.maxEntryBytes, entry.uncompressed + 1)) });
          body = bufferSource(inflated);
        } else {
          bad(file, `unsupported compression method ${entry.method}`); continue;
        }
        scanBody(body, file, ctx, depth);
      } catch (error) { bad(file, error.message); }
    }
  } catch (error) { bad(label, `invalid ZIP container (${error.message})`); }
}

// A byte source holding one file's content: ZIP -> recurse; native binary -> run the rules; anything else ignored.
function scanBody(body, file, ctx, depth) {
  const head = body.read(0, 4);
  if (isZipHeader(head)) { scanZip(body, file, ctx, depth + 1); return; }
  if (!isNativeBinaryHeader(head)) return;
  ctx.stats.binaries += 1;
  const scanner = new RuleScanner();
  for (let at = 0; at < body.size; at += CHUNK) scanner.push(body.read(at, Math.min(CHUNK, body.size - at)));
  for (const finding of scanner.findings({ allowContentOnlyDevtools: ctx.allowContentOnlyDevtools === true })) ctx.findings.push({ file, ...finding });
}

// ---- directory / file / container entry point ---------------------------------------------------------------
// Returns { findings, stats }. Fails closed: a path that cannot be read is a finding, not a pass. Directories (a
// `.app`, a staging dir) are walked. Symlinks are not followed (they would escape the bundle). A file with a ZIP
// header (MSIX, APPX, zip) is read through its central directory and each entry is scanned (stored or deflate).
export function scanWithStats(paths, options = {}) {
  const limits = limitsOf(options);
  const findings = [];
  const stats = newStats();
  const visit = (target) => {
    let stat;
    try { stat = lstatSync(target); } catch (error) { findings.push({ file: target, rule: "unscannable", reason: `could not scan: ${error.code ?? error.message}` }); return; }
    if (stat.isSymbolicLink()) return;
    if (stat.isDirectory()) {
      let names;
      try { names = readdirSync(target); } catch (error) { findings.push({ file: target, rule: "unscannable", reason: `could not list: ${error.code ?? error.message}` }); return; }
      for (const name of names.sort()) visit(path.join(target, name));
      return;
    }
    if (!stat.isFile()) return;
    let fd;
    try {
      const head = readHead(target);
      const isZip = isZipHeader(head);
      if (!isZip && !isNativeBinaryHeader(head)) {
        if (/\.dmg$/i.test(target)) stats.opaque.push({ file: target, kind: "dmg" });
        return;
      }
      fd = openSync(target, "r");
      const source = fdSource(fd, stat.size);
      if (isZip) { scanZip(source, target, { findings, stats, allowContentOnlyDevtools: options.allowContentOnlyDevtools === true, limits, budget: { left: limits.totalInflated } }, 0); return; }
      stats.binaries += 1;
      const scanner = new RuleScanner();
      for (let at = 0; at < stat.size; at += CHUNK) scanner.push(source.read(at, Math.min(CHUNK, stat.size - at)));
      if (looksLikeNsis(source, stat.size)) stats.opaque.push({ file: target, kind: "nsis" });
      for (const finding of scanner.findings({ allowContentOnlyDevtools: options.allowContentOnlyDevtools === true })) findings.push({ file: target, ...finding });
    } catch (error) {
      findings.push({ file: target, rule: "unscannable", reason: `could not scan: ${error.code ?? error.message}` });
    } finally {
      if (fd !== undefined) closeSync(fd);
    }
  };
  for (const target of paths) visit(path.resolve(target));
  return { findings, stats };
}

// NSIS installers carry the "NullsoftInst" signature in their first-stage header; the payload behind it is
// LZMA/zlib compressed, so the rules cannot see inside it.
const NSIS_SIGNATURE = Buffer.from("NullsoftInst", "latin1");
function looksLikeNsis(source, size) {
  const window = Math.min(size, 4 * 1024 * 1024);
  return source.read(0, window).includes(NSIS_SIGNATURE);
}

export function scanForQaControl(paths, options = {}) {
  return scanWithStats(paths, options).findings;
}

// `requireBinary`: fail when the scanned inputs contain no native binary at all (a missing or empty staging
// directory must not read as "clean"). Returns the scan stats so a caller can report what stayed unscannable.
export function assertNoQaControl(paths, label = "release", { requireBinary = false, limits, allowContentOnlyDevtools = false } = {}) {
  const { findings, stats } = scanWithStats(paths, { limits, allowContentOnlyDevtools });
  if (requireBinary && stats.binaries === 0 && findings.length === 0) {
    findings.push({ file: paths.join(", ") || "<none>", rule: "unscannable", reason: "no Mach-O, PE or ELF file found to scan (missing or empty staging input); cannot verify" });
  }
  if (findings.length === 0) return stats;
  throw new Error(
    `${label}: the QA control server and every other test-automation or debugging surface must never ship (wdio WebDriver plugin, remote debugging, Tauri devtools); `
    + `${findings.length} finding(s):\n${findings.map((f) => `- ${f.file}: ${f.reason}`).join("\n")}\n`
    + "Build release and update tiers without the app's qa-native, dev-native, qa-control and devtools features.",
  );
}

// ---- web-bundle mock scan (M20 owner gap) ----------------------------------------------------------------------
// The binary scan above is blind to the frontend bundle (a Vite `dist/`, a Tauri `frontendDist`, an installer
// payload folder). An app's in-page QA mock (HeardRight tauriQaMock, GenRight browser-mock-api, ScrapeRight and
// MailRight qaMock, PostRight MockDataSource) must never be reachable from a release bundle. Each marker is a
// string that survives minification (an identifier that is also a string literal, a file/chunk name, a protocol or
// a build-flag name). Source maps are scanned too: a map that names qaMock.ts proves the module was bundled.
export const WEB_MOCK_MARKERS = Object.freeze([
  { id: "browser-qa-fake", pattern: /browser-qa-fake/ },
  { id: "browser-mock-api", pattern: /browser-mock-api/ },
  { id: "browser-qa-mock-scheme", pattern: /browser-qa-mock:/ },
  { id: "qaMock", pattern: /qaMock/ },
  { id: "tauriQaMock", pattern: /tauriQaMock/ },
  { id: "isBrowserQaMode", pattern: /isBrowserQaMode/ },
  { id: "MockDataSource", pattern: /MockDataSource/ },
  { id: "qa-build-flag", pattern: /\bVITE_[A-Z0-9_]*_QA\b/ },
]);
const WEB_EXTENSIONS = /\.(?:m?js|cjs|html?|css|map)$/i;

export function webBundleMockFindings(text) {
  const out = [];
  for (const marker of WEB_MOCK_MARKERS) {
    const match = marker.pattern.exec(text);
    if (match) out.push({ rule: "web-bundle-mock", marker: marker.id, reason: `web bundle carries browser-QA mock marker \`${match[0]}\`` });
  }
  return out;
}

// Walks files and directories (symlinks not followed, as in the binary scan). Fails closed on an unreadable path
// and, with `requireFiles`, on an input that holds no scannable web file at all.
export function scanWebBundle(paths, { requireFiles = false } = {}) {
  const findings = []; let files = 0;
  const visit = (target) => {
    let stat;
    try { stat = lstatSync(target); } catch (error) { findings.push({ file: target, rule: "unscannable", reason: `could not scan: ${error.code ?? error.message}` }); return; }
    if (stat.isSymbolicLink()) return;
    if (stat.isDirectory()) {
      let names;
      try { names = readdirSync(target); } catch (error) { findings.push({ file: target, rule: "unscannable", reason: `could not list: ${error.code ?? error.message}` }); return; }
      for (const name of names.sort()) visit(path.join(target, name));
      return;
    }
    if (!stat.isFile() || !WEB_EXTENSIONS.test(target)) return;
    files += 1;
    try {
      for (const finding of webBundleMockFindings(readFileSync(target, "utf8"))) findings.push({ file: target, ...finding });
    } catch (error) { findings.push({ file: target, rule: "unscannable", reason: `could not scan: ${error.code ?? error.message}` }); }
  };
  for (const target of paths) visit(path.resolve(target));
  if (requireFiles && files === 0 && findings.length === 0) findings.push({ file: paths.join(", ") || "<none>", rule: "unscannable", reason: "no web bundle file (.js, .html, .css) found to scan; cannot verify" });
  return { findings, files };
}

export function assertNoWebBundleMocks(paths, label = "release", { requireFiles = false } = {}) {
  const { findings, files } = scanWebBundle(paths, { requireFiles });
  if (findings.length === 0) return { files };
  throw new Error(
    `${label}: a release web bundle must not contain browser-QA mocks or test-mode markers; ${findings.length} finding(s):\n`
    + `${findings.map((f) => `- ${f.file}: ${f.reason}`).join("\n")}\n`
    + "Exclude the mock from the production build (dynamic import behind a build-time constant that tree-shakes, or a separate entry).",
  );
}
