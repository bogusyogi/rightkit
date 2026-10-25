#!/usr/bin/env node
// `right-release license-notices binaries [--config <file>] [--output <file>] [--product-name <name>] [--receipt <path>] [--check]`:
// writes the notice appendix (one HTML file) for the binaries an app ships that no RightKit catalog row covers (a downloaded yt-dlp, a
// vendored tool, a command-built helper). Catalog-backed sidecars (ffmpeg, onnxruntime, pdfium, whisper) write their own notices when
// they are staged; everything else must be declared here, with its licence text, or the build has no right to ship it.
//
//   legal: { binaryNotices: { output: "legal/THIRD_PARTY_BINARIES.html", productName: "ScrapeRight", receipt: "src-tauri/binaries/stage-sidecar.json",
//            binaries: [{ name: "yt-dlp", version: "2026.03.17" | "self-updating", license: "Unlicense", sourceUrl: "https://github.com/yt-dlp/yt-dlp",
//                         files: ["legal/texts/yt-dlp-UNLICENSE.txt"], covers?: ["yt-dlp-helper"] }] } }
//
// Every `files` entry is an app-relative text file that must exist; no licence text is ever invented. With a `receipt` (the
// stage-sidecar receipt, also settable with `--receipt`) every staged file of kind remote, command, cmake or prebuilt must belong to a
// declared binary - by its entry name, or a name listed in `covers` - or the run fails naming the uncovered ones. The page records the
// receipt-independent inputs only (declared fields and the sha256 of each text), so the same declaration always produces the same bytes.
// `--check` writes nothing and exits 1 when the file on disk differs.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { CommandSpecError, loadAppConfig, relativeInside } from "./command-spec.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";
import { escapeHtml } from "./npm-license-notices.mjs";

const fail = (message) => { throw new CommandSpecError(message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const KEYS = ["output", "productName", "receipt", "binaries"];
const BINARY_KEYS = ["name", "version", "license", "sourceUrl", "files", "covers"];
/** Receipt kinds with no catalog row behind them. */
export const NON_CATALOG_KINDS = Object.freeze(["remote", "command", "cmake", "prebuilt"]);

/** Resolve and validate `legal.binaryNotices` plus flags. */
export function resolveBinaryNoticeOptions(config, flags = {}) {
  const declared = config?.legal?.binaryNotices;
  if (!isObject(declared)) fail("legal.binaryNotices must be an object");
  const unknown = Object.keys(declared).filter((key) => !KEYS.includes(key));
  if (unknown.length) fail(`legal.binaryNotices has unknown key(s): ${unknown.join(", ")} (allowed: ${KEYS.join(", ")})`);
  const output = flags.output ?? declared.output;
  if (output === undefined) fail("no output file: set legal.binaryNotices.output or pass --output");
  relativeInside(output, "legal.binaryNotices.output");
  const productName = flags.productName ?? declared.productName;
  if (typeof productName !== "string" || !productName.trim()) fail("no product name: set legal.binaryNotices.productName or pass --product-name");
  const receipt = flags.receipt ?? declared.receipt;
  if (receipt !== undefined) relativeInside(receipt, "legal.binaryNotices.receipt");
  if (!Array.isArray(declared.binaries) || !declared.binaries.length) fail("legal.binaryNotices.binaries must be a non-empty list");
  const names = new Set();
  const binaries = declared.binaries.map((item, index) => {
    const label = `legal.binaryNotices.binaries[${index}]`;
    if (!isObject(item)) fail(`${label} must be an object`);
    const extra = Object.keys(item).filter((key) => !BINARY_KEYS.includes(key));
    if (extra.length) fail(`${label} has unknown key(s): ${extra.join(", ")} (allowed: ${BINARY_KEYS.join(", ")})`);
    for (const key of ["name", "version", "license"]) if (typeof item[key] !== "string" || !item[key].trim()) fail(`${label}.${key} must be a non-empty string`);
    if (typeof item.sourceUrl !== "string" || !/^https:\/\/[^\s/]+\/\S*$/.test(item.sourceUrl)) fail(`${label}.sourceUrl must be an https:// URL of the source or release`);
    if (!Array.isArray(item.files) || !item.files.length) fail(`${label}.files must list at least one licence/notice text file`);
    const files = item.files.map((file, fileIndex) => relativeInside(file, `${label}.files[${fileIndex}]`));
    const covers = item.covers ?? [];
    if (!Array.isArray(covers) || covers.some((name) => typeof name !== "string" || !name.trim())) fail(`${label}.covers must be a list of staged entry names`);
    for (const name of [item.name, ...covers]) {
      if (names.has(name)) fail(`${label}: ${name} is declared twice`);
      names.add(name);
    }
    return { name: item.name, version: item.version, license: item.license, sourceUrl: item.sourceUrl, files, covers: [...covers] };
  });
  return { output, productName, receipt, binaries };
}

/** The names of staged non-catalog entries that no declared binary covers. */
export function uncoveredBinaries(receipt, binaries) {
  const covered = new Set(binaries.flatMap((binary) => [binary.name, ...(binary.covers ?? [])]));
  const missing = new Set();
  for (const file of receipt?.files ?? []) {
    if (!NON_CATALOG_KINDS.includes(file.kind)) continue;
    const name = file.entryName ?? file.command ?? file.name;
    if (!covered.has(name)) missing.add(name);
  }
  return [...missing].sort();
}

/** Render the appendix. `texts` maps an app-relative file to its text. Returns the HTML. */
export function renderBinaryNotices({ productName, binaries, texts }) {
  const sections = binaries.map((binary) => {
    const bodies = binary.files.map((file) => `<h3>${escapeHtml(path.posix.basename(file))}</h3><pre>${escapeHtml(texts.get(file))}</pre>`).join("\n");
    const digests = binary.files.map((file) => `${escapeHtml(path.posix.basename(file))} sha256=${createHash("sha256").update(texts.get(file)).digest("hex")}`).join("; ");
    return `<section><h2>${escapeHtml(binary.name)} ${escapeHtml(binary.version)}</h2><p>License: ${escapeHtml(binary.license)}</p><p>Source: ${escapeHtml(binary.sourceUrl)}</p><p>Texts: ${digests}</p>\n${bodies}</section>`;
  });
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>${escapeHtml(productName)} bundled program licenses</title></head><body>
<h1>${escapeHtml(productName)} bundled program licenses</h1>
<p>Programs shipped with ${escapeHtml(productName)} that are not Cargo or npm dependencies; generated-by=right-release license-notices binaries</p>
${sections.join("\n")}
</body></html>
`;
}

/** Generate (or check) the appendix for the app at `appRoot`. File access is injectable. */
export function generateBinaryNotices({ appRoot, config, flags = {}, check = false, exists = existsSync, readText = (file) => readFileSync(file, "utf8"), writeText = (file, text) => { mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, text); } }) {
  const { output, productName, receipt, binaries } = resolveBinaryNoticeOptions(config, flags);
  const texts = new Map();
  for (const binary of binaries) {
    for (const file of binary.files) {
      const absolute = path.join(appRoot, file);
      if (!exists(absolute)) fail(`${binary.name}: licence text ${file} does not exist; no text is invented, add the file`);
      const text = String(readText(absolute)).replace(/\r\n/g, "\n");
      if (!text.trim()) fail(`${binary.name}: licence text ${file} is empty`);
      texts.set(file, text);
    }
  }
  let uncovered = [];
  if (receipt !== undefined) {
    const receiptFile = path.join(appRoot, receipt);
    if (!exists(receiptFile)) fail(`the stage-sidecar receipt ${receipt} does not exist; run right-release stage-sidecar first`);
    let parsed;
    try { parsed = JSON.parse(readText(receiptFile)); } catch (error) { fail(`the stage-sidecar receipt ${receipt} is not JSON (${error.message})`); }
    uncovered = uncoveredBinaries(parsed, binaries);
    if (uncovered.length) fail(`staged program(s) with no declared licence notice: ${uncovered.join(", ")}; declare them in legal.binaryNotices.binaries (name or covers)`);
  }
  const html = renderBinaryNotices({ productName, binaries, texts });
  const target = path.join(appRoot, output);
  if (check) {
    const current = exists(target) ? String(readText(target)).replace(/\r\n/g, "\n") : null;
    return { target, binaryCount: binaries.length, uncovered, upToDate: current === html };
  }
  writeText(target, html);
  return { target, binaryCount: binaries.length, uncovered, upToDate: true };
}

export function parseBinaryNoticeArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), output: undefined, productName: undefined, receipt: undefined, check: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config" || arg === "--output" || arg === "--product-name" || arg === "--receipt") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg === "--product-name" ? "productName" : arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--check") opts.check = true;
    else throw new Error(`unknown license-notices argument: ${arg}`);
  }
  return opts;
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseBinaryNoticeArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  const outcome = generateBinaryNotices({ appRoot, config, flags: { output: opts.output, productName: opts.productName, receipt: opts.receipt }, check: opts.check });
  if (opts.check) {
    if (!outcome.upToDate) {
      console.error(`right-release license-notices: ${outcome.target} is out of date; run right-release license-notices binaries`);
      return 1;
    }
    console.log(`${outcome.target} is up to date (${outcome.binaryCount} program notices)`);
    return 0;
  }
  console.log(`wrote ${outcome.binaryCount} program notices to ${outcome.target}`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release license-notices: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
