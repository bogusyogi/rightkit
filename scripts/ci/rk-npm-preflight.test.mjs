import assert from "node:assert/strict";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { adapterExpectation, ADAPTER, npmRecipePolicy, packageRows, parseArgs, readPackages } from "./rk-npm-preflight.mjs";

test("arguments: npm registry and one @rightkit package only", () => {
  const opts = parseArgs(["--registry", "npm", "--unit", "@rightkit/legal"]);
  assert.equal(opts.unit, "@rightkit/legal");
  assert.throws(() => parseArgs(["--registry", "crates", "--unit", "rightkit-fs"]), /must be npm/);
  assert.throws(() => parseArgs(["--registry", "npm", "--unit", "left-pad"]), /@rightkit/);
  assert.throws(() => parseArgs(["--registry", "npm", "--unit", "@rightkit/legal", "--skip-tests"]), /unknown argument/);
});

test("rows mirror the hosted npm verify: frozen install pinned to the store, the build closure, then the package's test script", () => {
  const pkg = { name: "@rightkit/legal", dir: "packages/legal", version: "0.6.2", scripts: { test: "vitest run" } };
  const rows = packageRows("@rightkit/legal", pkg, { storeDir: "/Volumes/D/.pnpm-store/v11" });
  assert.deepEqual(rows.map((r) => r.id), ["install", "build-closure", "test"]);
  assert.deepEqual(rows[0].args, ["install", "--frozen-lockfile", "--store-dir", "/Volumes/D/.pnpm-store/v11"]);
  assert.ok(rows[1].args.includes("--filter") && rows[1].args.includes("@rightkit/legal...") && rows[1].args.slice(-2).join(" ") === "run build");
  assert.deepEqual(rows[2].args, ["--dir", "packages/legal", "run", "test"]);
  assert.throws(() => packageRows("@rightkit/legal", pkg, {}), /store pin/);
  assert.throws(() => packageRows("@rightkit/x", { ...pkg, scripts: {} }, { storeDir: "/s" }), /no test script/);
});

test("the attestation expectation binds the adapter id, this host's node and the pinned pnpm script", () => {
  const expect = adapterExpectation({ nodeSha256: "aa", pnpmScriptSha256: "bb", pnpmScriptPath: "/p/pnpm.mjs" });
  assert.deepEqual(expect, { kind: "adapter", adapter: ADAPTER, executable: { sha256: "aa" }, script: { path: "/p/pnpm.mjs", sha256: "bb" } });
});

test("workspace packages are read from packages/*/package.json with their directory", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-npm-"));
  fs.mkdirSync(path.join(root, "packages", "legal"), { recursive: true });
  fs.mkdirSync(path.join(root, "packages", "no-manifest"), { recursive: true });
  fs.writeFileSync(path.join(root, "packages", "legal", "package.json"), JSON.stringify({ name: "@rightkit/legal", version: "0.6.2", scripts: { test: "x" } }));
  const packages = readPackages(root);
  assert.deepEqual([...packages.keys()], ["@rightkit/legal"]);
  assert.equal(packages.get("@rightkit/legal").dir, "packages/legal");
  assert.equal(packages.get("@rightkit/legal").version, "0.6.2");
});


test("npm recipe policy mirrors the hosted refusal: a reviewed entry with matching file hashes passes; no entry, a changed file and a missing file refuse", () => {
  const crypto = require("node:crypto");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-npm-recipe-"));
  fs.mkdirSync(path.join(root, "ci", "consumers", "npm", "x"), { recursive: true });
  fs.writeFileSync(path.join(root, "ci", "consumers", "npm", "x", "consumer.mjs"), "console.log(1)\n");
  const sha = crypto.createHash("sha256").update("console.log(1)\n").digest("hex");
  const write = (inv) => fs.writeFileSync(path.join(root, "ci", "consumer-recipes.json"), JSON.stringify(inv));
  write({ schema: 1, npm: {} });
  assert.match(npmRecipePolicy(root, "@rightkit/x").reason, /missing reviewed PK example/);
  write({ schema: 1, npm: { "@rightkit/x": { pkId: "PK90", id: "ci/consumers/npm/x/consumer.mjs", files: { "ci/consumers/npm/x/consumer.mjs": sha } } } });
  assert.deepEqual(npmRecipePolicy(root, "@rightkit/x"), { ok: true, pkId: "PK90", id: "ci/consumers/npm/x/consumer.mjs", files: 1 });
  fs.writeFileSync(path.join(root, "ci", "consumers", "npm", "x", "consumer.mjs"), "console.log(2)\n");
  assert.match(npmRecipePolicy(root, "@rightkit/x").reason, /differs from reviewed inventory: ci\/consumers\/npm\/x\/consumer.mjs/);
  fs.rmSync(path.join(root, "ci", "consumers", "npm", "x", "consumer.mjs"));
  assert.match(npmRecipePolicy(root, "@rightkit/x").reason, /missing or not a regular file/);
});

test("@rightkit/release is refused while its bundled version feed is behind the registries; other units are not checked", async () => {
  const { releaseFeedPolicy } = await import("./rk-npm-preflight.mjs");
  const behind = async () => ({ changes: [{ registry: "cargo", name: "rightkit-qa", from: "0.7.1", to: "0.8.0" }] });
  const current = async () => ({ changes: [] });
  const offline = async () => { throw new Error("fetch failed"); };
  const r = await releaseFeedPolicy("/x", "@rightkit/release", { sync: behind });
  assert.equal(r.ok, false);
  assert.match(r.reason, /cargo rightkit-qa 0\.7\.1 -> 0\.8\.0/);
  assert.equal((await releaseFeedPolicy("/x", "@rightkit/release", { sync: current })).ok, true);
  assert.equal((await releaseFeedPolicy("/x", "@rightkit/release", { sync: offline })).ok, false, "an unreachable registry fails closed");
  assert.deepEqual(await releaseFeedPolicy("/x", "@rightkit/qa", { sync: behind }), { ok: true, skipped: true });
});

// ---- @rightkit/release pipeline canary policy and version pins ----
import crypto from "node:crypto";
import { CANARY_DIGEST_FILES, CANARY_EXPECTED_FILE, CANARY_RECEIPT, releaseCanaryPolicy } from "./rk-npm-preflight.mjs";
import { DIGEST_FILES, EXPECTED_FILE } from "./rk-release-canary.mjs";

const REPO_ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
function canaryFixture(t, { verdict = "green", sha = "a".repeat(40), version = "0.6.3", receiptSha = sha, receiptVersion = version, ok = verdict === "green", writeReceipt = true } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-canary-policy-"));
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "rk-canary-out-"));
  t.after(() => { fs.rmSync(root, { recursive: true, force: true }); fs.rmSync(out, { recursive: true, force: true }); });
  const digest = (rel) => crypto.createHash("sha256").update(fs.readFileSync(path.join(root, rel))).digest("hex");
  for (const rel of [...CANARY_DIGEST_FILES, CANARY_EXPECTED_FILE]) { fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); fs.copyFileSync(path.join(REPO_ROOT, rel), path.join(root, rel)); }
  const receipt = { schema: "rk-release-canary.v1", unit: "@rightkit/release", sha: receiptSha, version: receiptVersion, verdict, ok, digests: { scripts: Object.fromEntries(CANARY_DIGEST_FILES.map((f) => [f, digest(f)])), expected: digest(CANARY_EXPECTED_FILE) } };
  if (writeReceipt) { fs.mkdirSync(path.join(out, sha), { recursive: true }); fs.writeFileSync(path.join(out, sha, CANARY_RECEIPT), JSON.stringify(receipt)); }
  return { root, out, sha, version, receipt };
}

test("the pre-flight's canary digest list is the canary's own list", () => {
  assert.deepEqual([...CANARY_DIGEST_FILES], [...DIGEST_FILES]);
  assert.equal(CANARY_EXPECTED_FILE, EXPECTED_FILE);
  assert.equal(CANARY_RECEIPT, "@rightkit__release.canary.json");
});

test("@rightkit/release pre-flight: a green canary for this SHA, version, scripts and expected file passes", (t) => {
  const f = canaryFixture(t);
  const r = releaseCanaryPolicy(f.root, "@rightkit/release", { sha: f.sha, version: f.version, out: f.out });
  assert.equal(r.ok, true);
  assert.equal(r.verdict, "green");
});

test("@rightkit/release pre-flight: no receipt, a receipt for another SHA, another version, or a non-green verdict refuses", (t) => {
  const none = canaryFixture(t, { writeReceipt: false });
  assert.match(releaseCanaryPolicy(none.root, "@rightkit/release", { sha: none.sha, version: none.version, out: none.out }).reason, /no canary receipt/);
  const other = canaryFixture(t, { receiptSha: "b".repeat(40) });
  assert.match(releaseCanaryPolicy(other.root, "@rightkit/release", { sha: other.sha, version: other.version, out: other.out }).reason, /is for bbbbbbbbbbbb, not aaaaaaaaaaaa/);
  const stale = canaryFixture(t, { receiptVersion: "0.6.2" });
  assert.match(releaseCanaryPolicy(stale.root, "@rightkit/release", { sha: stale.sha, version: stale.version, out: stale.out }).reason, /0\.6\.2, not 0\.6\.3/);
  for (const verdict of ["incomplete", "regression"]) {
    const f = canaryFixture(t, { verdict });
    assert.match(releaseCanaryPolicy(f.root, "@rightkit/release", { sha: f.sha, version: f.version, out: f.out }).reason, /not green/);
  }
  const lying = canaryFixture(t, { verdict: "green", ok: false });
  assert.equal(releaseCanaryPolicy(lying.root, "@rightkit/release", { sha: lying.sha, version: lying.version, out: lying.out }).ok, false);
});

test("@rightkit/release pre-flight: a receipt written by different canary scripts or judged against a different expected file refuses", (t) => {
  const a = canaryFixture(t);
  fs.appendFileSync(path.join(a.root, "scripts/ci/rk-release-canary.mjs"), "\n// changed after the canary ran\n");
  assert.match(releaseCanaryPolicy(a.root, "@rightkit/release", { sha: a.sha, version: a.version, out: a.out }).reason, /different scripts\/ci\/rk-release-canary\.mjs/);
  const b = canaryFixture(t);
  fs.writeFileSync(path.join(b.root, CANARY_EXPECTED_FILE), JSON.stringify({ schema: 1, entries: [] }, null, 4));
  assert.match(releaseCanaryPolicy(b.root, "@rightkit/release", { sha: b.sha, version: b.version, out: b.out }).reason, /different ci\/release-canary-expected\.json/);
  const c = canaryFixture(t);
  fs.appendFileSync(path.join(c.root, "ci/release-canary.json"), "\n");
  assert.match(releaseCanaryPolicy(c.root, "@rightkit/release", { sha: c.sha, version: c.version, out: c.out }).reason, /different ci\/release-canary\.json/);
});

test("other units are unaffected by the canary policy", (t) => {
  const none = canaryFixture(t, { writeReceipt: false });
  assert.deepEqual(releaseCanaryPolicy(none.root, "@rightkit/qa", { sha: none.sha, version: "1.0.0", out: none.out }), { ok: true, skipped: true });
  assert.deepEqual(releaseCanaryPolicy(none.root, "@rightkit/legal", { sha: none.sha, version: "1.0.0", out: none.out }), { ok: true, skipped: true });
});
