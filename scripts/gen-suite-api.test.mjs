import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { CLIENT_PATH_CONST, CONTRACT_PATH, MJS_PATH, RUST_PATH, SWIFT_PATH, TEST_VECTOR_PATH, TS_PATH, minisignPublicKeyId, notAfterUnix, parseContract, renderProjections, suiteApiProblems } from "./gen-suite-api.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SCRIPT = path.join(ROOT, "scripts", "gen-suite-api.mjs");
const HOST = "api.orthiclabs.com";
const LEGACY = "api.spoares.com";
const PROJECTIONS = [RUST_PATH, TS_PATH, MJS_PATH, SWIFT_PATH, TEST_VECTOR_PATH];
const real = (rel) => readFileSync(path.join(ROOT, rel), "utf8");

// A temporary RightKit tree holding only the contract. Tests never touch the real generated files.
function tempRoot() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-suite-api-"));
  mkdirSync(path.join(dir, "contracts"), { recursive: true });
  copyFileSync(path.join(ROOT, CONTRACT_PATH), path.join(dir, CONTRACT_PATH));
  return dir;
}
const run = (root, ...args) => spawnSync(process.execPath, [SCRIPT, "--root", root, ...args], { encoding: "utf8" });
const readAll = (root) => Object.fromEntries(PROJECTIONS.map((p) => [p, readFileSync(path.join(root, p), "utf8")]));

test("the generator is deterministic: two runs over the same contract write identical bytes", () => {
  const a = tempRoot(); const b = tempRoot();
  try {
    assert.equal(run(a).status, 0);
    assert.equal(run(b).status, 0);
    assert.deepEqual(readAll(a), readAll(b));
    assert.equal(renderProjections(real(CONTRACT_PATH)).get(RUST_PATH), readAll(a)[RUST_PATH]);
    for (const text of Object.values(readAll(a))) {
      assert.ok(!text.includes("\r"), "LF line endings only");
      assert.ok(text.endsWith("\n") && !text.endsWith("\n\n"), "exactly one trailing newline");
    }
  } finally {
    rmSync(a, { recursive: true, force: true }); rmSync(b, { recursive: true, force: true });
  }
});

test("--check passes after generation and writes nothing", () => {
  const root = tempRoot();
  try {
    assert.equal(run(root).status, 0);
    const before = readAll(root);
    const check = run(root, "--check");
    assert.equal(check.status, 0, check.stderr);
    assert.deepEqual(readAll(root), before);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("--check fails after a one-byte edit to the contract and after a one-byte edit to a projection", () => {
  const root = tempRoot();
  try {
    assert.equal(run(root).status, 0);
    const contractPath = path.join(root, CONTRACT_PATH);
    const original = readFileSync(contractPath, "utf8");
    const edited = original.replace('"Canonical', '"canonical');
    assert.equal(edited.length, original.length);
    assert.equal([...original].filter((ch, i) => ch !== edited[i]).length, 1, "exactly one byte differs");
    writeFileSync(contractPath, edited);
    const afterContractEdit = run(root, "--check");
    assert.equal(afterContractEdit.status, 1);
    assert.match(afterContractEdit.stderr, new RegExp(TEST_VECTOR_PATH.replaceAll(".", "\\.")));
    writeFileSync(contractPath, original);

    const tsPath = path.join(root, TS_PATH);
    const ts = readFileSync(tsPath, "utf8");
    writeFileSync(tsPath, ts.replace(`"${HOST}";`, `"${HOST} ";`));
    const afterProjectionEdit = run(root, "--check");
    assert.equal(afterProjectionEdit.status, 1);
    assert.match(afterProjectionEdit.stderr, new RegExp(TS_PATH.replaceAll(".", "\\.").replaceAll("/", "\\/")));
    assert.equal(readFileSync(tsPath, "utf8").includes(`"${HOST} ";`), true, "--check never rewrites a projection");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("each projection contains the contract host", () => {
  for (const p of PROJECTIONS) assert.ok(real(p).includes(HOST), `${p} carries ${HOST}`);
});

test("the generated Rust and TS mention the legacy host only in the LEGACY_HOSTS constant", () => {
  const lines = (p) => real(p).split("\n").filter((line) => line.includes(LEGACY));
  assert.deepEqual(lines(RUST_PATH), [`pub const LEGACY_HOSTS: &[&str] = &[${JSON.stringify(LEGACY)}];`]);
  assert.deepEqual(lines(TS_PATH), [`export const RIGHT_SUITE_LEGACY_HOSTS = [${JSON.stringify(LEGACY)}] as const;`]);
  assert.deepEqual(lines(MJS_PATH), [`export const RIGHT_SUITE_LEGACY_HOSTS = [${JSON.stringify(LEGACY)}];`]);
});

test("the test-vector copy is byte-identical to the contract", () => {
  assert.equal(real(TEST_VECTOR_PATH), real(CONTRACT_PATH));
});

test("suiteApiProblems reports a missing projection and passes a complete set", () => {
  const files = renderProjections(real(CONTRACT_PATH));
  files.set(CONTRACT_PATH, real(CONTRACT_PATH));
  const readFrom = (map) => (rel) => { if (!map.has(rel)) throw new Error(`ENOENT ${rel}`); return map.get(rel); };
  assert.deepEqual(suiteApiProblems(readFrom(files)), []);
  files.delete(MJS_PATH);
  const problems = suiteApiProblems(readFrom(files));
  assert.equal(problems.length, 1);
  assert.match(problems[0], /suite-api\.generated\.mjs: missing/);
});

test("parseContract refuses a legacy host equal to the host, a foreign schema and an unknown artifact set", () => {
  const c = JSON.parse(real(CONTRACT_PATH));
  assert.throws(() => parseContract(JSON.stringify({ ...c, legacyHosts: [HOST] })), /legacyHosts/);
  assert.throws(() => parseContract(JSON.stringify({ ...c, schema: "right-suite-api.v2" })), /schema/);
  assert.throws(() => parseContract(JSON.stringify({ ...c, apps: { viewright: { artifactHosts: ["other"] } } })), /artifactHosts/);
});

// A syntactically valid minisign public key (as tauri.conf.json carries it) for a made-up key id. It is not a key pair: the
// generator only reads the format and the key id.
function fakeUpdaterKey(kid) {
  const raw = Buffer.concat([Buffer.from("Ed"), Buffer.from(kid, "hex").reverse(), Buffer.alloc(32, 7)]);
  return Buffer.from(`untrusted comment: minisign public key: ${kid}\n${raw.toString("base64")}\n`).toString("base64");
}
const contractWith = (mutate) => { const c = JSON.parse(real(CONTRACT_PATH)); mutate(c); return JSON.stringify(c, null, 2); };
const SIGNING_KID = JSON.parse(real(CONTRACT_PATH)).updaterSigningKeyId;
const ROTATED_KID = "0123456789ABCDEF";
const twoKeys = (c) => { c.updaterPublicKeys.push({ kid: ROTATED_KID, publicKey: fakeUpdaterKey(ROTATED_KID) }); };

test("the contract's updater key is a minisign key whose id is its kid, and the licence key is 32 bytes", () => {
  const c = JSON.parse(real(CONTRACT_PATH));
  assert.equal(minisignPublicKeyId(c.updaterPublicKeys[0].publicKey), c.updaterPublicKeys[0].kid);
  assert.equal(c.updaterSigningKeyId, c.updaterPublicKeys[0].kid);
  assert.equal(Buffer.from(c.licensePublicKeyHex, "hex").length, 32);
  assert.equal(minisignPublicKeyId(fakeUpdaterKey(ROTATED_KID)), ROTATED_KID);
});

test("minisignPublicKeyId reads nothing from text that is not a minisign key or whose id disagrees", () => {
  const good = JSON.parse(real(CONTRACT_PATH)).updaterPublicKeys[0].publicKey;
  const text = Buffer.from(good, "base64").toString("utf8");
  const wrongComment = Buffer.from(text.replace(SIGNING_KID, "FFFFFFFFFFFFFFFF")).toString("base64");
  for (const bad of ["", "PLACEHOLDER-REPLACE-WITH-SUITE-UPDATER-PUBKEY", "not base64!", good.slice(0, -4), wrongComment, Buffer.from("untrusted comment: x\nAAAA\n").toString("base64")]) {
    assert.equal(minisignPublicKeyId(bad), null, bad.slice(0, 40));
  }
  assert.equal(minisignPublicKeyId(undefined), null);
});

test("notAfterUnix is the first second the key is refused, and only a real UTC day in range is a date", () => {
  assert.equal(notAfterUnix("2027-01-01"), Date.UTC(2027, 0, 2) / 1000);
  assert.equal(notAfterUnix("2028-02-29"), Date.UTC(2028, 1, 29) / 1000 + 86400);
  for (const bad of ["2027-02-30", "2027-13-01", "2025-12-31", "2101-01-01", "2027-1-1", "2027-01-01T00:00:00Z", "", null, 20270101]) assert.equal(notAfterUnix(bad), null, String(bad));
});

test("parseContract refuses a malformed licence key, key list, registry entry or signing key", () => {
  const refuse = (mutate, pattern) => assert.throws(() => parseContract(contractWith(mutate)), pattern);
  refuse((c) => { c.licensePublicKeyHex = c.licensePublicKeyHex.slice(0, 62); }, /licensePublicKeyHex/);
  refuse((c) => { c.licensePublicKeyHex = c.licensePublicKeyHex.toUpperCase(); }, /licensePublicKeyHex/);
  refuse((c) => { c.licensePublicKeyHex = "0".repeat(64); }, /licensePublicKeyHex/);
  refuse((c) => { delete c.licensePublicKeyHex; }, /licensePublicKeyHex/);
  refuse((c) => { c.updaterPublicKeys = []; }, /updaterPublicKeys/);
  refuse((c) => { delete c.updaterPublicKeys; }, /updaterPublicKeys/);
  refuse((c) => { c.updaterPublicKeys.push({ ...c.updaterPublicKeys[0] }); }, /listed twice/);
  refuse((c) => { c.updaterPublicKeys.push({ kid: ROTATED_KID, publicKey: fakeUpdaterKey("FEDCBA9876543210") }); }, /minisign public key/);
  refuse((c) => { c.updaterPublicKeys[0].publicKey = "PLACEHOLDER-REPLACE-WITH-SUITE-UPDATER-PUBKEY"; }, /minisign public key/);
  refuse((c) => { c.updaterPublicKeys[0].kid = "29395f9ff466261d"; }, /kid/);
  refuse((c) => { c.updaterPublicKeys[0].extra = 1; }, /may only have/);
  refuse((c) => { twoKeys(c); c.updaterPublicKeys[1].notAfter = "2027-02-30"; }, /notAfter/);
  refuse((c) => { c.updaterSigningKeyId = ROTATED_KID; }, /updaterSigningKeyId/);
  refuse((c) => { delete c.updaterSigningKeyId; }, /updaterSigningKeyId/);
  refuse((c) => { c.updaterPublicKeys[0].notAfter = "2030-01-01"; }, /signing key must not have a notAfter/);
  refuse((c) => { c.apps.viewright.extra = 1; }, /apps\.viewright may only have/);
  refuse((c) => { c.apps.viewright.artifactHosts = []; }, /artifactHosts/);
  refuse((c) => { c.apps.viewright.licenseHost = c.host; }, /licenseHost/);
  refuse((c) => { c.apps.viewright.licenseHost = c.artifactHostSets.public; }, /licenseHost/);
  refuse((c) => { c.apps.viewright.licenseHost = "not a host"; }, /licenseHost/);
  refuse((c) => { c.pendingNativeUpdater.push(c.pendingNativeUpdater[0]); }, /without duplicates/);
  refuse((c) => { c.pendingNativeUpdater.push("noapp"); }, /pendingNativeUpdater/);
});

test("an app may leave out artifactHosts or licenseHost, and then no projection invents one", () => {
  const c = JSON.parse(real(CONTRACT_PATH));
  const bare = Object.keys(c.apps).filter((key) => c.apps[key].licenseHost === undefined);
  assert.ok(bare.length > 0, "the registry lists at least one app that has not declared a licence host");
  const files = renderProjections(real(CONTRACT_PATH));
  for (const app of bare) {
    assert.ok(!files.get(MJS_PATH).includes(`"${app}": "`), `${app} has no licence host in the ESM projection`);
    assert.ok(!files.get(SWIFT_PATH).includes(`"${app}": "`), `${app} has no licence host in the Swift projection`);
  }
  const noArtifacts = Object.keys(c.apps).filter((key) => c.apps[key].artifactHosts === undefined);
  assert.ok(noArtifacts.length > 0);
  for (const app of noArtifacts) assert.ok(!files.get(RUST_PATH).includes(`(${JSON.stringify(app)}, &[`), `${app} has no artifact hosts`);
});

test("every projection carries the licence key, the signing key id and the key list the contract holds", () => {
  const c = JSON.parse(real(CONTRACT_PATH));
  const files = renderProjections(real(CONTRACT_PATH));
  const base64url = Buffer.from(c.licensePublicKeyHex, "hex").toString("base64url");
  for (const [file, text] of files) {
    if (file === TEST_VECTOR_PATH) continue;
    assert.ok(text.includes(c.updaterSigningKeyId), `${file} names the signing key id`);
    assert.ok(text.includes(c.updaterPublicKeys[0].publicKey), `${file} carries the updater key`);
    assert.ok(text.includes(file === SWIFT_PATH ? base64url : c.licensePublicKeyHex), `${file} carries the licence key`);
  }
  assert.match(files.get(RUST_PATH), /pub const UPDATER_PUBLIC_KEYS: &\[\(&str, &str, Option<i64>\)\] = &\[\n    \("29395F9FF466261D", "[A-Za-z0-9+/=]+", None\),\n\];/);
  assert.match(files.get(MJS_PATH), /"notAfter": null,\n    "notAfterUnix": null/);
});

test("a rotation contract (two keys, the old one expiring) projects the same key list to every language", () => {
  const text = contractWith((c) => { twoKeys(c); c.updaterPublicKeys[0].notAfter = undefined; c.updaterSigningKeyId = SIGNING_KID; c.updaterPublicKeys[1].notAfter = "2027-06-30"; });
  const files = renderProjections(text);
  const unix = Date.UTC(2027, 5, 30) / 1000 + 86400;
  assert.match(files.get(RUST_PATH), new RegExp(`\\("${ROTATED_KID}", "[A-Za-z0-9+/=]+", Some\\(${unix}\\)\\),`));
  assert.match(files.get(MJS_PATH), new RegExp(`"kid": "${ROTATED_KID}",\\n    "publicKey": "[A-Za-z0-9+/=]+",\\n    "notAfter": "2027-06-30",\\n    "notAfterUnix": ${unix}`));
  assert.match(files.get(TS_PATH), /RIGHT_SUITE_UPDATER_PUBLIC_KEYS = \[[\s\S]*\] as const;/);
  assert.match(files.get(SWIFT_PATH), new RegExp(`RightSuiteUpdaterPublicKey\\(keyId: "${ROTATED_KID}", publicKey: "[A-Za-z0-9+/=]+", notAfterUnix: ${unix}\\)`));
});

test("the generated Rust is what rustfmt writes, for the contract and for a rotation contract", (t) => {
  const probe = spawnSync("rustfmt", ["--version"], { encoding: "utf8" });
  if (probe.error || probe.status !== 0) { t.skip("rustfmt is not installed here; CI's fmt gate checks the generated file"); return; }
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-suite-api-fmt-"));
  try {
    for (const text of [real(CONTRACT_PATH), contractWith((c) => { twoKeys(c); c.updaterPublicKeys[1].notAfter = "2027-06-30"; })]) {
      const file = path.join(dir, "suite_api_generated.rs");
      writeFileSync(file, renderProjections(text).get(RUST_PATH));
      const result = spawnSync("rustfmt", ["--edition", "2021", "--check", file], { encoding: "utf8" });
      assert.equal(result.status, 0, result.stdout + result.stderr);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the registry lists every suite app of the mandatory set, and only Ember (its own updater key) is outside it", () => {
  const mandatory = JSON.parse(real("packages/release/ownership-mandatory-set.json"));
  const outsideSuite = ["ember"];
  const expected = mandatory.defaultApps.filter((app) => !outsideSuite.includes(app)).sort();
  assert.deepEqual(Object.keys(JSON.parse(real(CONTRACT_PATH)).apps).sort(), expected);
});

test("every route the native licence client calls comes from the contract: no /v1 literal outside tests, every constant used", () => {
  const sources = ["client/wire.rs", "client/account.rs", "subject.rs"].map((file) => {
    const text = real(`crates/rightkit-license/src/${file}`);
    // Production code only: cut the unit-test module, then every comment (doc text may show a route).
    const production = text.split(/\n#\[cfg\(test\)\]\nmod tests/)[0];
    return { file, code: production.split("\n").filter((line) => !/^\s*\/\//.test(line)).join("\n") };
  });
  for (const { file, code } of sources) {
    assert.doesNotMatch(code, /"[^"\n]*\/v1\//, `${file} has a hand-written /v1 route; use the generated constant from suite_api`);
  }
  const all = sources.map((s) => s.code).join("\n");
  for (const name of Object.values(CLIENT_PATH_CONST)) {
    assert.match(all, new RegExp(`suite_api::${name}\\b`), `${name} is used by the client`);
  }
  // The constants carry what the contract says, so a typo is one edit in one place.
  const c = JSON.parse(real(CONTRACT_PATH));
  const rust = real(RUST_PATH);
  for (const [key, name] of Object.entries(CLIENT_PATH_CONST)) {
    assert.ok(rust.includes(`pub const ${name}: &str = ${JSON.stringify(c.clientPaths[key])};`), `${name} projects clientPaths.${key}`);
  }
});

test("parseContract refuses client paths that are missing, repeated, off /v1 or carry another placeholder", () => {
  const refuse = (mutate) => assert.throws(() => parseContract(contractWith(mutate)), /clientPaths/);
  refuse((c) => { delete c.clientPaths.leaseIssue; });
  refuse((c) => { c.clientPaths.extra = "/v1/extra"; });
  refuse((c) => { c.clientPaths.licenseTrial = c.clientPaths.licenseActivate; });
  refuse((c) => { c.clientPaths.licenseTrial = "/v2/licenses/trial"; });
  refuse((c) => { c.clientPaths.licenseTrial = "/v1/{device}/trial"; });
  refuse((c) => { c.clientPaths.licenseTrial = "/v1/licenses/trial?x=1"; });
  refuse((c) => { delete c.clientPaths; });
});

test("the signature vectors are one file: the contracts/ original and the crate's byte copy are identical", () => {
  assert.equal(real("crates/rightkit-license/test-vectors/updater-signature-vectors.v1.json"), real("contracts/updater-signature-vectors.v1.json"));
});

test(".gitattributes pins the contract, every projection and the shared signature vectors to LF: a Windows checkout would break the byte comparisons", () => {
  const pinned = [CONTRACT_PATH, ...PROJECTIONS, "contracts/updater-signature-vectors.v1.json", "crates/rightkit-license/test-vectors/updater-signature-vectors.v1.json"];
  const attrs = spawnSync("git", ["check-attr", "eol", "--", ...pinned], { cwd: ROOT, encoding: "utf8" });
  assert.equal(attrs.status, 0, attrs.stderr);
  assert.deepEqual(attrs.stdout.trim().split("\n").filter((line) => !line.endsWith(": eol: lf")), [], "each is `eol=lf`");
});
