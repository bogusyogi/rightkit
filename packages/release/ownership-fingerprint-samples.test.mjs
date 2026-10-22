import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { MANIFEST_FILE, scanApp } from "./ownership.mjs";
import { FINGERPRINT_SAMPLES } from "./fixtures/ownership/source-fingerprint-samples.mjs";

const catalog = JSON.parse(readFileSync(path.join(import.meta.dirname, MANIFEST_FILE), "utf8"));
const sourceFingerprints = catalog.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "source").map((f) => ({ ...f, owner: c.package })));
const sampled = sourceFingerprints.filter((f) => FINGERPRINT_SAMPLES[f.id]);

function repo(files) {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rk-fp-samples-")));
  assert.equal(spawnSync("git", ["-C", root, "init", "-q"], { windowsHide: true }).status, 0);
  for (const [rel, text] of Object.entries(files)) { mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); writeFileSync(path.join(root, rel), text); }
  return root;
}

test("every sampled source fingerprint is advisory, code-only and owned by its declaring package", () => {
  assert.ok(sampled.length >= 30, `expected the added source fingerprints, found ${sampled.length}`);
  for (const f of sampled) { const webview = f.id.startsWith("oauth.webview."); assert.equal(f.tier, webview ? "block" : "advise", f.id); assert.equal(f.codeOnly, !webview, f.id); assert.ok(f.extensions.length, f.id); }
  assert.deepEqual(Object.keys(FINGERPRINT_SAMPLES).filter((id) => !sampled.some((f) => f.id === id)), [], "sample without a catalog fingerprint");
});

// One repository per assertion family, every sample a file of its own: the scanner reports findings per file, so the
// per-file checks below are the same as scanning each sample alone, at three `git init` + scans instead of several hundred.
const slug = (id) => id.replace(/[^A-Za-z0-9]+/g, "-");
const samplesRepo = (kind) => {
  const files = {}; const expected = [];
  for (const f of sampled) for (const [index, sample] of FINGERPRINT_SAMPLES[f.id][kind].entries()) {
    const rel = `src/${kind}-${slug(f.id)}-${index}${f.extensions[0]}`;
    files[rel] = `${sample}\n`; expected.push({ f, rel, sample });
  }
  return { root: repo(files), expected };
};

test("added source fingerprints find hand-written reimplementations through the real scanner", () => {
  const { root, expected } = samplesRepo("positive");
  try {
    const found = scanApp(root, catalog);
    for (const { f, rel, sample } of expected) assert.ok(found.some((x) => x.fingerprint === f.id && x.file === rel), `${f.id} misses: ${sample}`);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("added source fingerprints ignore normal RightKit usage, comments, literals and unrelated code", () => {
  const { root, expected } = samplesRepo("negative");
  try {
    const found = scanApp(root, catalog);
    for (const { f, rel, sample } of expected) assert.equal(found.filter((x) => x.fingerprint === f.id && x.file === rel).length, 0, `${f.id} matches: ${sample}`);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("source fingerprints stay quiet in extensions they do not declare", () => {
  const files = {}; const expected = [];
  for (const f of sampled) {
    const other = [".rs", ".ts", ".swift", ".py"].find((ext) => !f.extensions.includes(ext));
    if (!other) continue;
    const rel = `src/other-${slug(f.id)}${other}`;
    files[rel] = `${FINGERPRINT_SAMPLES[f.id].positive[0]}\n`; expected.push({ f, rel, other });
  }
  const root = repo(files);
  try {
    const found = scanApp(root, catalog);
    for (const { f, rel, other } of expected) assert.equal(found.filter((x) => x.fingerprint === f.id && x.file === rel).length, 0, `${f.id} fired in ${other}`);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("added dependency fingerprints are found only as direct manifest dependencies", () => {
  const deps = catalog.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "dependency" && /^(?:fsindex|jobs|license|logs|search|keychain|app-shell|onboarding|theme|qa-chrome|release)\./.test(f.id)).map((f) => ({ ...f, owner: c.package })));
  assert.ok(deps.length >= 40);
  for (const f of deps) assert.equal(f.tier, "advise", f.id);
  const cargo = deps.find((f) => f.ecosystem === "cargo"), npm = deps.find((f) => f.ecosystem === "npm");
  const root = repo({
    "Cargo.toml": `[package]\nname="app"\nversion="0.1.0"\n[dependencies]\n${cargo.name}="1"\n`,
    "Cargo.lock": `version = 4\n[[package]]\nname = "app"\nversion = "0.1.0"\ndependencies = ["${cargo.name}"]\n[[package]]\nname = "${cargo.name}"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n`,
    "package.json": JSON.stringify({ name: "app", version: "1.0.0", dependencies: { [npm.name]: "1.0.0" } }),
    "package-lock.json": JSON.stringify({ name: "app", version: "1.0.0", lockfileVersion: 3, packages: { "": { name: "app", version: "1.0.0", dependencies: { [npm.name]: "1.0.0" } }, [`node_modules/${npm.name}`]: { version: "1.0.0" } } }),
  });
  try {
    const found = scanApp(root, catalog);
    assert.ok(found.some((x) => x.fingerprint === cargo.id), cargo.id);
    assert.ok(found.some((x) => x.fingerprint === npm.id), npm.id);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
