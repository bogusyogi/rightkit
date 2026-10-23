#!/usr/bin/env node
// Keep every Chrome for Testing pin on Google's current Stable (Adrian, 2026-10-23: "Chrome is always at the latest
// update ... cannot be one whole release behind").
//
//   node scripts/ci/chrome-update.mjs --check            print `current <v>` or `outdated <pinned> -> <stable>`; exit 0 / 3
//   node scripts/ci/chrome-update.mjs --apply [--force]  move every pin to Stable with digests computed from the real archives
//
// One version, three pin sources, always together:
//   ci/chrome-for-testing.json                      the hosted/local qualification pin: archive sha256 + extracted executable sha256
//   crates/rightkit-browser/src/install.rs          what apps install at run time (ensure_chrome_for_testing)
//   packages/qa-chrome/src/install.ts               what QA tooling installs
// Stable comes from Google's last-known-good-versions.json. Every archive is downloaded once from the official
// chrome-for-testing-public bucket over HTTPS (no redirects off it), hashed, and its executable member extracted and
// hashed; nothing is taken from a third party or from a previous pin. The Rust and TypeScript tables are written by the
// reviewed scripts/chrome-pins.mjs --apply; tests that assert the pinned URL follow the version. A package whose current
// version is already on its registry gets a patch bump and a changelog line, so the new pin ships as a new version.
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const LKG_URL = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json";
export const BUCKET = "https://storage.googleapis.com/chrome-for-testing-public/";
const VERSION_RE = /^\d+\.\d+\.\d+\.\d+$/;

export function compareVersions(a, b) {
  const x = a.split(".").map(Number), y = b.split(".").map(Number);
  for (let i = 0; i < 4; i += 1) if (x[i] !== y[i]) return x[i] - y[i];
  return 0;
}

export async function stableVersion(fetchImpl = fetch) {
  const r = await fetchImpl(LKG_URL, { redirect: "error", signal: AbortSignal.timeout(60_000) });
  if (!r.ok) throw new Error(`last-known-good-versions: HTTP ${r.status}`);
  const version = (await r.json())?.channels?.Stable?.version;
  if (!VERSION_RE.test(version ?? "")) throw new Error(`last-known-good-versions: no Stable version (${JSON.stringify(version)})`);
  return version;
}

export const archiveUrl = (version, platform, kind) => `${BUCKET}${version}/${platform}/${kind === "headless" ? "chrome-headless-shell" : "chrome"}-${platform}.zip`;
/** The executable inside each archive, as provision-chrome.mjs executable()/shellExecutable() resolve it after extraction. */
export function executableMember(platform, kind) {
  if (kind === "headless") return `chrome-headless-shell-${platform}/${platform.startsWith("win") ? "chrome-headless-shell.exe" : "chrome-headless-shell"}`;
  if (platform.startsWith("mac")) return `chrome-${platform}/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing`;
  return `chrome-${platform}/${platform.startsWith("win") ? "chrome.exe" : "chrome"}`;
}

async function download(url, file, fetchImpl) {
  if (!url.startsWith(BUCKET)) throw new Error(`refusing a URL outside ${BUCKET}: ${url}`);
  const r = await fetchImpl(url, { redirect: "error", signal: AbortSignal.timeout(900_000) });
  if (!r.ok || !r.body) throw new Error(`${url}: HTTP ${r.status}`);
  const hash = crypto.createHash("sha256"); let size = 0;
  const out = fs.createWriteStream(file);
  for await (const chunk of r.body) { hash.update(chunk); size += chunk.length; if (!out.write(chunk)) await new Promise((res) => out.once("drain", res)); }
  await new Promise((res) => out.end(res));
  return { sha256: hash.digest("hex"), size };
}

function memberSha256(zip, member) {
  const r = spawnSync("unzip", ["-p", zip, member], { maxBuffer: 1024 * 1024 * 1024 });
  if (r.status !== 0 || !r.stdout?.length) throw new Error(`${path.basename(zip)}: cannot extract ${member}`);
  return crypto.createHash("sha256").update(r.stdout).digest("hex");
}

function bumpPatch(version) { const [a, b, c] = version.split(".").map(Number); return `${a}.${b}.${c + 1}`; }
function changelogLine(file, version, text) {
  if (!fs.existsSync(file)) return;
  const s = fs.readFileSync(file, "utf8");
  const at = s.search(/^## /m);
  const entry = `## ${version}\n\n- ${text}\n\n`;
  fs.writeFileSync(file, at < 0 ? `${s.trimEnd()}\n\n${entry}` : `${s.slice(0, at)}${entry}${s.slice(at)}`);
}

async function published(registry, name, version, fetchImpl) {
  const url = registry === "crates" ? `https://index.crates.io/${name.length <= 3 ? `${name.length}/${name}` : `${name.slice(0, 2)}/${name.slice(2, 4)}/${name}`}` : `https://registry.npmjs.org/${name.replace("/", "%2F")}`;
  const r = await fetchImpl(url, { headers: { "user-agent": "rightkit chrome-update (adrian@orthiclabs.com)" }, signal: AbortSignal.timeout(60_000) });
  if (r.status === 404) return false;
  if (!r.ok) throw new Error(`${name}: registry read failed (HTTP ${r.status})`);
  const text = await r.text();
  return registry === "crates" ? text.split("\n").filter(Boolean).some((l) => JSON.parse(l).vers === version) : Object.hasOwn(JSON.parse(text).versions ?? {}, version);
}

export async function applyUpdate({ root = ROOT, version, fetchImpl = fetch, log = console.log, tmp = fs.mkdtempSync(path.join(process.env.RUNNER_TEMP || os.tmpdir(), "rk-chrome-update-")) }) {
  const ciFile = path.join(root, "ci", "chrome-for-testing.json");
  const ci = JSON.parse(fs.readFileSync(ciFile, "utf8"));
  const previous = ci.version;
  // Every digest the sources pin today, keyed by archive, so tests that mirror a pin follow it.
  const oldDigests = new Map();
  for (const [p, e] of Object.entries(ci.platforms)) { oldDigests.set(`chrome/${p}/archive`, e.sha256); oldDigests.set(`chrome/${p}/exe`, e.executableSha256); }
  for (const [p, e] of Object.entries(ci.headlessShell ?? {})) { oldDigests.set(`headless/${p}/archive`, e.sha256); oldDigests.set(`headless/${p}/exe`, e.executableSha256); }
  const before = spawnSync(process.execPath, [path.join(root, "scripts", "chrome-pins.mjs")], { cwd: root, encoding: "utf8" });
  if (before.status === 0) for (const p of JSON.parse(before.stdout)) if (p.sha256) oldDigests.set(`${p.kind === "chrome" ? "chrome" : "headless"}/${p.platform}/archive`, p.sha256);
  // Every (kind, platform) any source pins.
  const tsv = spawnSync(process.execPath, [path.join(root, "scripts", "chrome-pins.mjs"), "--tsv"], { cwd: root, encoding: "utf8" });
  if (tsv.status !== 0) throw new Error(`chrome-pins --tsv: ${tsv.stderr.trim()}`);
  const wanted = new Map();
  for (const line of tsv.stdout.trim().split("\n")) { const [kind, platform] = line.split("\t"); wanted.set(`${kind === "chrome" ? "chrome" : "headless"}/${platform}`, { kind: kind === "chrome" ? "chrome" : "headless", platform, sourceKind: kind }); }
  for (const p of Object.keys(ci.platforms)) if (!wanted.has(`chrome/${p}`)) wanted.set(`chrome/${p}`, { kind: "chrome", platform: p, sourceKind: "chrome" });
  for (const p of Object.keys(ci.headlessShell ?? {})) if (!wanted.has(`headless/${p}`)) wanted.set(`headless/${p}`, { kind: "headless", platform: p, sourceKind: "headless-shell" });
  const digests = new Map();
  for (const [key, w] of wanted) {
    const url = archiveUrl(version, w.platform, w.kind);
    const file = path.join(tmp, `${w.kind}-${w.platform}.zip`);
    const { sha256, size } = await download(url, file, fetchImpl);
    const executableSha256 = memberSha256(file, executableMember(w.platform, w.kind));
    digests.set(key, { ...w, url, sha256, size, executableSha256 });
    log(`  ${key}: ${sha256} (${size} bytes), executable ${executableSha256}`);
    fs.rmSync(file, { force: true });
  }
  // 1. CI pin.
  ci.version = version;
  for (const p of Object.keys(ci.platforms)) { const d = digests.get(`chrome/${p}`); ci.platforms[p] = { ...ci.platforms[p], sha256: d.sha256, executableSha256: d.executableSha256 }; }
  for (const p of Object.keys(ci.headlessShell ?? {})) { const d = digests.get(`headless/${p}`); ci.headlessShell[p] = { ...ci.headlessShell[p], sha256: d.sha256, executableSha256: d.executableSha256 }; }
  fs.writeFileSync(ciFile, `${JSON.stringify(ci, null, 2)}\n`);
  // 2. Rust + TypeScript tables: move the version, then let the reviewed applier write every digest.
  const sources = ["crates/rightkit-browser/src/install.rs", "packages/qa-chrome/src/install.ts", "packages/qa-chrome/src/install.test.ts", "packages/qa-chrome/src/index.test.ts"];
  for (const rel of sources) { const f = path.join(root, rel); if (fs.existsSync(f)) fs.writeFileSync(f, fs.readFileSync(f, "utf8").split(previous).join(version)); }
  const pinsJson = path.join(tmp, "chrome-pins.json");
  const listed = spawnSync(process.execPath, [path.join(root, "scripts", "chrome-pins.mjs")], { cwd: root, encoding: "utf8" });
  if (listed.status !== 0) throw new Error(`chrome-pins after the version move: ${listed.stderr.trim()}`);
  const entries = JSON.parse(listed.stdout).map((p) => { const d = digests.get(`${p.kind === "chrome" ? "chrome" : "headless"}/${p.platform}`); return { kind: p.kind, platform: p.platform, version, url: p.url, sha256: d.sha256, size: d.size }; });
  fs.writeFileSync(pinsJson, JSON.stringify(entries));
  const applied = spawnSync(process.execPath, [path.join(root, "scripts", "chrome-pins.mjs"), "--apply", pinsJson], { cwd: root, encoding: "utf8" });
  if (applied.status !== 0) throw new Error(`chrome-pins --apply: ${applied.stderr.trim()}`);
  log(applied.stdout.trim());
  // Tests that assert a pinned digest literally follow it (each mirror names the archive whose pin it checks).
  const mirrors = ["scripts/ci/publish.test.mjs", "packages/qa-chrome/src/install.test.ts"];
  for (const rel of mirrors) {
    const f = path.join(root, rel); if (!fs.existsSync(f)) continue;
    let text = fs.readFileSync(f, "utf8");
    for (const [key, old] of oldDigests) {
      const [kind, platform, which] = key.split("/"); const d = digests.get(`${kind}/${platform}`);
      if (old && d) text = text.split(old).join(which === "exe" ? d.executableSha256 : d.sha256);
    }
    fs.writeFileSync(f, text);
  }
  // 3. A published version cannot change: bump the units whose pin moved, with a changelog line.
  const note = `Chrome for Testing ${previous} -> ${version} (current Stable): archive and executable SHA-256 pins recomputed from the official downloads.`;
  const bumped = [];
  const crateToml = path.join(root, "crates", "rightkit-browser", "Cargo.toml");
  const crateVersion = fs.readFileSync(crateToml, "utf8").match(/^version\s*=\s*"([^"]+)"/m)[1];
  if (await published("crates", "rightkit-browser", crateVersion, fetchImpl)) {
    const next = bumpPatch(crateVersion);
    fs.writeFileSync(crateToml, fs.readFileSync(crateToml, "utf8").replace(/^version\s*=\s*"[^"]+"/m, `version = "${next}"`));
    changelogLine(path.join(root, "crates", "rightkit-browser", "CHANGELOG.md"), next, note); bumped.push(`rightkit-browser ${next}`);
  } else { changelogLine(path.join(root, "crates", "rightkit-browser", "CHANGELOG.md"), `${crateVersion} (unreleased)`, note); }
  const pkgFile = path.join(root, "packages", "qa-chrome", "package.json");
  const pkg = JSON.parse(fs.readFileSync(pkgFile, "utf8"));
  if (await published("npm", pkg.name, pkg.version, fetchImpl)) {
    pkg.version = bumpPatch(pkg.version); fs.writeFileSync(pkgFile, `${JSON.stringify(pkg, null, 2)}\n`);
    changelogLine(path.join(root, "packages", "qa-chrome", "CHANGELOG.md"), pkg.version, note); bumped.push(`${pkg.name} ${pkg.version}`);
    // @rightkit/qa pins qa-chrome exactly: follow it, and its reviewed PK14 recipe (version, closure pin, hashes).
    const qaFile = path.join(root, "packages", "qa", "package.json");
    const qa = JSON.parse(fs.readFileSync(qaFile, "utf8"));
    const oldChrome = qa.dependencies?.["@rightkit/qa-chrome"];
    if (oldChrome && oldChrome !== pkg.version) {
      const oldQa = qa.version;
      qa.dependencies["@rightkit/qa-chrome"] = pkg.version;
      if (await published("npm", qa.name, qa.version, fetchImpl)) qa.version = bumpPatch(qa.version);
      fs.writeFileSync(qaFile, `${JSON.stringify(qa, null, 2)}\n`);
      changelogLine(path.join(root, "packages", "qa", "CHANGELOG.md"), qa.version, `Depends on @rightkit/qa-chrome ${pkg.version}: ${note}`);
      bumped.push(`${qa.name} ${qa.version}`);
      const recipe = path.join(root, "ci", "consumers", "npm", "qa");
      const edit = (rel, pairs) => { const f = path.join(recipe, rel); if (!fs.existsSync(f)) return; let t = fs.readFileSync(f, "utf8"); for (const [a, b] of pairs) t = t.split(a).join(b); fs.writeFileSync(f, t); };
      edit("consumer.mjs", [[`EXPECTED_VERSION = '${oldQa}'`, `EXPECTED_VERSION = '${qa.version}'`]]);
      edit("README.md", [[`| \`${oldQa}\` |`, `| \`${qa.version}\` |`], [oldChrome, pkg.version]]);
      edit("package.json", [[`"@rightkit/qa-chrome": "${oldChrome}"`, `"@rightkit/qa-chrome": "${pkg.version}"`]]);
      const invFile = path.join(root, "ci", "consumer-recipes.json");
      const inv = JSON.parse(fs.readFileSync(invFile, "utf8"));
      const entry = inv.npm?.["@rightkit/qa"];
      if (entry) {
        if (entry.closure?.install?.["@rightkit/qa-chrome"]) entry.closure.install["@rightkit/qa-chrome"] = pkg.version;
        for (const f of Object.keys(entry.files)) entry.files[f] = crypto.createHash("sha256").update(fs.readFileSync(path.join(root, f))).digest("hex");
        fs.writeFileSync(invFile, `${JSON.stringify(inv, null, 2)}\n`);
      }
    }
  }
  fs.rmSync(tmp, { recursive: true, force: true });
  return { previous, version, digests: [...digests.values()], bumped };
}

async function main(argv) {
  const ci = JSON.parse(fs.readFileSync(path.join(ROOT, "ci", "chrome-for-testing.json"), "utf8"));
  const stable = await stableVersion();
  const outdated = compareVersions(stable, ci.version) > 0;
  if (argv.includes("--check")) { console.log(outdated ? `outdated ${ci.version} -> ${stable}` : `current ${ci.version}`); return outdated ? 3 : 0; }
  if (!argv.includes("--apply")) { console.error("usage: chrome-update.mjs --check | --apply [--force]"); return 2; }
  if (!outdated && !argv.includes("--force")) { console.log(`current ${ci.version}`); return 0; }
  console.log(`updating Chrome for Testing ${ci.version} -> ${stable}`);
  const r = await applyUpdate({ version: stable });
  console.log(`updated ${r.previous} -> ${r.version}; bumped: ${r.bumped.join(", ") || "none (unpublished versions carry the pin)"}`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (error) => { console.error(`chrome-update: ${error.message}`); process.exit(1); });
}
