import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { evaluateWindowsAcl, validateTrustRegistry } from "./ownership-approval.mjs";
import { createKeyEvent, keyFingerprint, verifyKeyLog } from "./ownership-key-log.mjs";
import { __testOnly, mergeRepositoriesFromInstalledRelease, prepareTrustUpdate, REPOSITORY_DRIFT_CODE, REPOSITORY_REGISTRY_MALFORMED_CODE } from "./ownership-key-trust.mjs";

function makeKey(platform = "windows", deviceName = "Test device") {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  return { privateKey: pair.privateKey, record: { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform, deviceName } };
}
// Match key-log's canonical JSON encoding for nested payloads.
function canonical(value) {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
}
function appendSigned(log, signer, operation, subject) {
  const draft = createKeyEvent(log, { operation, subject, signerKeyId: signer.record.keyId });
  return { schema: 1, events: [...log.events, { ...draft, signature: { keyId: signer.record.keyId, host: signer.record.platform, value: sign("sha256", Buffer.from(canonical(draft.payload)), signer.privateKey).toString("base64") } }] };
}
function fixture() {
  const root = makeKey("windows", "Root device");
  const extra = makeKey("macos", "Second device");
  const empty = { schema: 1, events: [] };
  const first = appendSigned(empty, root, "root", root.record);
  const second = appendSigned(first, root, "add", extra.record);
  return { root, first, second, pin: root.record.keyId };
}
const baseline = { schema: 1, minimumScannerVersion: "1.2", minimumCatalogVersion: "3", revokedKeyIds: [], verifiedTrackingEvidence: [], repositories: [], provenance: [], hosts: { retained: { label: "keep" } }, vendorPolicy: { enabled: true } };

test("first pin installs verified checkpoint while preserving policy baseline", () => {
  const { first, pin } = fixture();
  const updated = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline });
  assert.equal(updated.rootFingerprint, pin);
  assert.deepEqual(updated.keyCheckpoint, verifyKeyLog(first, { rootFingerprint: pin }).checkpoint);
  assert.deepEqual(updated.repositories, baseline.repositories);
  assert.deepEqual(updated.hosts, baseline.hosts);
  assert.deepEqual(updated.vendorPolicy, baseline.vendorPolicy);
});

test("same root advances checkpoint while retaining every existing policy field", () => {
  const { first, second, pin } = fixture();
  const initial = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline });
  const advanced = prepareTrustUpdate(initial, { log: second, rootFingerprint: pin, baseline: { schema: 1 } });
  assert.deepEqual(advanced.keyCheckpoint, verifyKeyLog(second, { rootFingerprint: pin }).checkpoint);
  assert.deepEqual(advanced.repositories, baseline.repositories);
  assert.deepEqual(advanced.hosts, baseline.hosts);
  assert.deepEqual(advanced.vendorPolicy, baseline.vendorPolicy);
});

test("first pin preserves valid legacy protected policy without prior key checkpoint", () => {
  const { first, pin } = fixture();
  const legacy = { ...baseline, hosts: { windows: { legacyKeyId: "keep" } } };
  const updated = prepareTrustUpdate(legacy, { log: first, rootFingerprint: pin });
  assert.equal(updated.rootFingerprint, pin);
  assert.deepEqual(updated.keyCheckpoint, verifyKeyLog(first, { rootFingerprint: pin }).checkpoint);
  assert.deepEqual(updated.hosts, legacy.hosts);
  assert.throws(() => prepareTrustUpdate({ ...legacy, rootFingerprint: pin }, { log: first, rootFingerprint: pin }), /lacks checkpoint/);
});

test("authority pins exact repository and checkout, survives omitted updates, and rejects relocation", () => {
  const { first, second, pin } = fixture();
  const checkoutPath = path.resolve(process.cwd());
  const authority = { repositoryIdentity: "https://github.com/orthic-labs/claude", checkoutPath };
  const initial = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline, authority });
  assert.deepEqual(initial.keyAuthority, authority);
  const advanced = prepareTrustUpdate(initial, { log: second, rootFingerprint: pin });
  assert.deepEqual(advanced.keyAuthority, authority);
  const pathAlias = process.platform === "win32" ? checkoutPath.toUpperCase() : `${checkoutPath}/.`;
  const retained = prepareTrustUpdate(initial, { log: second, rootFingerprint: pin, authority: { ...authority, checkoutPath: pathAlias } });
  assert.equal(retained.keyAuthority.checkoutPath, authority.checkoutPath);
  assert.throws(() => prepareTrustUpdate(initial, { log: second, rootFingerprint: pin, authority: { ...authority, repositoryIdentity: "https://github.com/orthic-labs/other" } }), /authority differs/);
  assert.throws(() => prepareTrustUpdate(initial, { log: second, rootFingerprint: pin, authority: { ...authority, checkoutPath: path.join(checkoutPath, "other") } }), /authority differs/);
  assert.throws(() => prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline, authority: { ...authority, extra: true } }), /exactly repositoryIdentity/);
});

test("committed-authority verification binds origin, checkout root, and request log to committed bytes", () => {
  const { first, pin } = fixture();
  // The authority must name its canonical path (macOS tmpdir is a /var -> /private/var symlink).
  const checkout = realpathSync.native(mkdtempSync(path.join(tmpdir(), "rk-key-authority-")));
  const logBytes = Buffer.from(canonical(first));
  mkdirSync(path.join(checkout, ".rightkit"));
  writeFileSync(path.join(checkout, ".rightkit", "ownership-keys.json"), logBytes);
  const authority = { repositoryIdentity: "https://github.com/orthic-labs/claude", checkoutPath: checkout };
  const sha = "a".repeat(40);
  const calls = [];
  let treeMode = "100644";
  const runner = (command, args, options) => {
    calls.push({ command, args, options });
    const tail = args.slice(args.indexOf("-C") + 2);
    if (tail[0] === "rev-parse" && tail[1] === "--show-toplevel") return { status: 0, stdout: Buffer.from(checkout) };
    if (tail[0] === "config") return { status: 0, stdout: Buffer.from("https://github.com/orthic-labs/claude") };
    if (tail[0] === "rev-parse" && tail[1] === "--verify" && tail[2] === "HEAD^{commit}") return { status: 0, stdout: Buffer.from(sha) };
    if (tail[0] === "rev-parse" && tail[2] === `${sha}^{tree}`) return { status: 0, stdout: Buffer.from(sha) };
    if (tail[0] === "ls-tree") return { status: 0, stdout: Buffer.from(`${treeMode} blob ${sha}\t.rightkit/ownership-keys.json\0`) };
    if (tail[0] === "show") return { status: 0, stdout: logBytes };
    return { status: 1, stdout: Buffer.alloc(0) };
  };
  try {
    const checkpoint = verifyKeyLog(first, { rootFingerprint: pin }).checkpoint;
    const receipt = __testOnly.validateCommittedAuthority(authority, first, pin, checkpoint, runner);
  assert.deepEqual(receipt, { repositoryIdentity: authority.repositoryIdentity, commit: sha, tree: sha });
    assert.ok(calls.every(({ command, args, options }) => command === (process.platform === "win32" ? "C:\\Program Files\\Git\\cmd\\git.exe" : "/usr/bin/git")
      && args[0] === "--no-replace-objects" && args.includes("core.fsmonitor=false") && options.env.PATH !== process.env.PATH
      && options.env.GIT_CONFIG_NOSYSTEM === "1" && options.env.GIT_TERMINAL_PROMPT === "0"));
    assert.ok(calls.some(({ args }) => args.includes(`${sha}^{tree}`)));
    assert.ok(calls.some(({ args }) => args.includes(`${sha}:.rightkit/ownership-keys.json`)));
    assert.ok(calls.every(({ args }) => !args.includes("HEAD^{tree}") && !args.includes("HEAD:.rightkit/ownership-keys.json")));
    assert.throws(() => __testOnly.validateCommittedAuthority(authority, { ...first, events: [] }, pin, checkpoint, runner), /differs from committed/);
    treeMode = "160000";
    assert.throws(() => __testOnly.validateCommittedAuthority(authority, first, pin, checkpoint, runner), /regular blob/);
    treeMode = "100644";
    writeFileSync(path.join(checkout, ".rightkit", "ownership-keys.json"), canonical({ schema: 1, events: [] }));
    assert.throws(() => __testOnly.validateCommittedAuthority(authority, first, pin, checkpoint, runner), /differs from committed bytes/);
  } finally { rmSync(checkout, { recursive: true, force: true }); }
});

test("rejects root rotation, checkpoint rollback, fork, and baseline pin injection", () => {
  const { first, second, pin } = fixture();
  const current = prepareTrustUpdate(null, { log: second, rootFingerprint: pin, baseline });
  assert.throws(() => prepareTrustUpdate(current, { log: second, rootFingerprint: "f".repeat(64), baseline }), /root pin differs/);
  assert.throws(() => prepareTrustUpdate(current, { log: first, rootFingerprint: pin, baseline }), /shorter than protected checkpoint/);
  const other = fixture();
  assert.throws(() => prepareTrustUpdate(current, { log: other.second, rootFingerprint: pin, baseline }), /signature is invalid|root fingerprint/);
  assert.throws(() => prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline: { ...baseline, rootFingerprint: pin } }), /policy only/);
});

test("Windows ACL evaluator permits non-elevated trust reads and rejects user writes", () => {
  const readAcl = { ownerSID: "S-1-5-18", ownerName: "SYSTEM", attributes: "Archive", directory: false, elevated: false, access: [
    { identitySID: "S-1-5-18", type: "Allow", mask: 2032127, propagationFlags: 0 },
    { identitySID: "S-1-5-32-544", type: "Allow", mask: 2032127, propagationFlags: 0 },
    { identitySID: "S-1-5-32-545", type: "Allow", mask: 1179817, propagationFlags: 0 },
  ] };
  assert.equal(evaluateWindowsAcl(readAcl, { target: "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership\\trust.json" }).ok, true);
  assert.equal(evaluateWindowsAcl({ ...readAcl, ownerSID: "S-1-5-32-545" }, { target: "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership\\trust.json" }).ok, false);
  assert.equal(evaluateWindowsAcl({ ...readAcl, access: [...readAcl.access.slice(0, 2), { ...readAcl.access[2], mask: 1179819 }] }, { target: "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership\\trust.json" }).ok, false);
});

test("disposable Windows ACL setup seam grants reader access without invoking OS changes", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "rk-trust-acl-"));
  const calls = [];
  try {
    const installed = __testOnly.runWindowsAclSetupInDisposableDirectory(directory, (command, args, options) => {
      calls.push({ command, args, options });
      return { status: 0 };
    });
    assert.equal(installed.file, path.join(directory, "trust.json"));
    assert.equal(calls.length, 1);
    assert.equal(calls[0].options.env.RK_TRUST_DIR, directory);
    assert.equal(calls[0].options.env.RK_TRUST_FILE, installed.file);
    assert.equal(Object.keys(calls[0].options.env).some((key) => key.toLowerCase() === "psmodulepath"), false);
    assert.match(calls[0].args[3], /ReadAndExecute/);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test("Windows directory ACL evaluator rejects untrusted write access on descendants", () => {
  const acl = { ownerSID: "S-1-5-18", ownerName: "SYSTEM", attributes: "Directory", directory: true, nonempty: true, elevated: false, access: [
    { identitySID: "S-1-5-18", type: "Allow", mask: 2032127, propagationFlags: 0 },
    { identitySID: "S-1-5-32-544", type: "Allow", mask: 2032127, propagationFlags: 0 },
    { identitySID: "S-1-5-32-545", type: "Allow", mask: 2, propagationFlags: 0 },
  ] };
  assert.equal(evaluateWindowsAcl(acl, { target: "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership" }).ok, false);
  assert.match(evaluateWindowsAcl(acl, { target: "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership" }).problems.join(" "), /untrusted principal/);
});

test("elevated updates raise floors to the installed release and never lower them", async () => {
  const { raiseFloorsToInstalledRelease } = await import("./ownership-key-trust.mjs");
  const manifest = { schema: 1, version: "3.0.0", capabilities: [] };
  const base = { schema: 1, minimumScannerVersion: "2.2.0", minimumCatalogVersion: "1", minimumCatalogDigest: "a".repeat(64), revokedKeyIds: [], verifiedTrackingEvidence: [], repositories: [], provenance: [] };
  const raised = raiseFloorsToInstalledRelease(base, { manifest, scannerVersion: "2.6.0" });
  assert.equal(raised.minimumCatalogVersion, "3.0.0");
  assert.equal(raised.minimumScannerVersion, "2.6.0");
  assert.notEqual(raised.minimumCatalogDigest, base.minimumCatalogDigest);
  assert.deepEqual(raiseFloorsToInstalledRelease({ ...base, minimumCatalogVersion: "4" }, { manifest, scannerVersion: "2.6.0" }), { ...base, minimumCatalogVersion: "4" });
  assert.deepEqual(raiseFloorsToInstalledRelease({ ...base, minimumScannerVersion: "3.0.0" }, { manifest, scannerVersion: "2.6.0" }), { ...base, minimumScannerVersion: "3.0.0" });
});

const CODERIGHT = "https://github.com/bogusyogi/coderight";
const COCKPIT = "https://github.com/orthic-labs/cockpit";
const repo = (repositoryIdentity, visibility = "private", roots = ["."]) => ({ repositoryIdentity, roots, visibility });
const registryOf = (repositories) => ({ schema: 1, repositories });
const trustWith = (repositories) => ({
  schema: 1, minimumScannerVersion: "1.2", minimumCatalogVersion: "3", revokedKeyIds: [], verifiedTrackingEvidence: [],
  repositories, provenance: [{ repositoryIdentity: CODERIGHT, paths: ["vendor/x.js"], contentDigest: "b".repeat(64), kind: "third-party", requestId: "req-1" }], hosts: { keep: { label: "keep" } },
});

test("registry merge adds a new registration and leaves existing entries, provenance and other fields untouched", () => {
  const existing = trustWith([repo(CODERIGHT)]);
  const snapshot = structuredClone(existing);
  const { trust: merged, warnings } = mergeRepositoriesFromInstalledRelease(existing, { registry: registryOf([repo(CODERIGHT), repo(COCKPIT, "public")]) });
  assert.deepEqual(merged.repositories, [repo(CODERIGHT), repo(COCKPIT, "public")]);
  assert.deepEqual({ ...merged, repositories: undefined }, { ...snapshot, repositories: undefined });
  assert.deepEqual(warnings, []);
  assert.deepEqual(existing, snapshot, "input trust must not be mutated");
});

test("registry drift keeps the protected entry, skips that registry entry with a typed warning, and still adds others", () => {
  const { first, second, pin } = fixture();
  const current = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline: { ...baseline, repositories: [repo(COCKPIT, "private")] } });
  const advanced = prepareTrustUpdate(current, { log: second, rootFingerprint: pin, baseline: { schema: 1 } });
  const legion = repo("https://github.com/orthic-labs/legion", "public");
  const { trust: merged, warnings } = mergeRepositoriesFromInstalledRelease(advanced, { registry: registryOf([repo(COCKPIT, "public"), legion]) });
  assert.deepEqual(merged.repositories, [repo(COCKPIT, "private"), legion]);
  assert.equal(warnings.length, 1);
  assert.equal(warnings[0].code, REPOSITORY_DRIFT_CODE);
  assert.equal(warnings[0].type, REPOSITORY_DRIFT_CODE);
  assert.equal(warnings[0].repositoryIdentity, COCKPIT);
  assert.match(warnings[0].message, /orthic-labs\/cockpit/);
  // The checkpoint of the committed key log still applies alongside the drift warning.
  assert.deepEqual(merged.keyCheckpoint, verifyKeyLog(second, { rootFingerprint: pin }).checkpoint);
});

test("roots drift is also a warning, not a refusal", () => {
  const rootsDrift = trustWith([repo(COCKPIT, "public", ["packages"])]);
  const { trust, warnings } = mergeRepositoriesFromInstalledRelease(rootsDrift, { registry: registryOf([repo(COCKPIT, "public", ["."])]) });
  assert.deepEqual(trust.repositories, [repo(COCKPIT, "public", ["packages"])]);
  assert.deepEqual(warnings.map((w) => w.code), [REPOSITORY_DRIFT_CODE]);
});

test("a revocation-shaped update proceeds despite registry drift", () => {
  const { root, second, pin } = fixture();
  const revoked = appendSigned(second, root, "revoke", { keyId: second.events[1].payload.subject.keyId });
  const current = prepareTrustUpdate(null, { log: second, rootFingerprint: pin, baseline: { ...baseline, repositories: [repo(COCKPIT, "private")] } });
  const prepared = prepareTrustUpdate(current, { log: revoked, rootFingerprint: pin, baseline: { schema: 1 } });
  const { trust, warnings } = mergeRepositoriesFromInstalledRelease(prepared, { registry: registryOf([repo(COCKPIT, "public")]) });
  assert.deepEqual(trust.keyCheckpoint, verifyKeyLog(revoked, { rootFingerprint: pin }).checkpoint);
  assert.equal(warnings[0].code, REPOSITORY_DRIFT_CODE);
});

test("a malformed registry skips the whole registry merge with a typed warning while the trust update proceeds", () => {
  const existing = trustWith([repo(CODERIGHT)]);
  const snapshot = structuredClone(existing);
  const good = repo("https://github.com/orthic-labs/legion", "public");
  const malformed = {
    "wrong schema": { schema: 2, repositories: [good] },
    "extra registry key": { schema: 1, repositories: [good], provenance: [] },
    "extra entry field": { schema: 1, repositories: [{ ...good, owner: "x" }] },
    "noncanonical identity": { schema: 1, repositories: [repo("https://github.com/Orthic-Labs/legion", "public")] },
    "duplicate identity": { schema: 1, repositories: [good, { ...good, visibility: "private" }] },
    "bad visibility": { schema: 1, repositories: [repo("https://github.com/orthic-labs/legion", "internal")] },
    "absolute root": { schema: 1, repositories: [repo("https://github.com/orthic-labs/legion", "public", ["/etc"])] },
    "parent root": { schema: 1, repositories: [repo("https://github.com/orthic-labs/legion", "public", ["../x"])] },
    "glob root": { schema: 1, repositories: [repo("https://github.com/orthic-labs/legion", "public", ["*"])] },
    "empty roots": { schema: 1, repositories: [repo("https://github.com/orthic-labs/legion", "public", [])] },
    "empty list": { schema: 1, repositories: [] },
    "non-array list": { schema: 1, repositories: {} },
    "partial valid plus invalid": { schema: 1, repositories: [good, repo("https://github.com/orthic-labs/x", "bogus")] },
  };
  for (const [label, registry] of Object.entries(malformed)) {
    const { trust, warnings } = mergeRepositoriesFromInstalledRelease(existing, { registry });
    assert.deepEqual(trust.repositories, existing.repositories, label);
    assert.deepEqual(warnings.map((w) => w.code), [REPOSITORY_REGISTRY_MALFORMED_CODE], label);
  }
  assert.deepEqual(existing, snapshot);
});

test("request-supplied baseline registrations cannot enter protected registry", () => {
  const { first, second, pin } = fixture();
  const evil = repo("https://github.com/attacker/payload", "public");
  const registry = registryOf([repo(CODERIGHT)]);
  // First install: the baseline is ignored for registrations, only the committed registry seeds them.
  const freshPrepared = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline: { ...baseline, repositories: [evil] } });
  assert.deepEqual(mergeRepositoriesFromInstalledRelease(freshPrepared, { firstInstall: true, registry }).trust.repositories, [repo(CODERIGHT)]);
  // Later update: request policy fields cannot add registrations to an existing protected trust.
  const current = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline });
  const advanced = prepareTrustUpdate(current, { log: second, rootFingerprint: pin, baseline: { schema: 1, repositories: [evil] } });
  assert.deepEqual(mergeRepositoriesFromInstalledRelease(advanced, { registry }).trust.repositories, [repo(CODERIGHT)]);
  assert.equal(mergeRepositoriesFromInstalledRelease(advanced, { registry }).trust.repositories.some((x) => x.repositoryIdentity === evil.repositoryIdentity), false);
});

test("seeded ownership-repositories.json validates; the default policy carries no registrations", () => {
  const seeded = JSON.parse(readFileSync(new URL("./ownership-repositories.json", import.meta.url), "utf8"));
  const policy = JSON.parse(readFileSync(new URL("./ownership-default-policy.json", import.meta.url), "utf8"));
  assert.equal(validateTrustRegistry({ ...policy, repositories: seeded.repositories }).ok, true);
  assert.deepEqual(policy.repositories, []);
  assert.equal(seeded.repositories.length, 14);
  assert.deepEqual(seeded.repositories.slice(9).map((x) => [x.repositoryIdentity, x.visibility]), [
    ["https://github.com/bogusyogi/postright", "private"],
    ["https://github.com/orthic-labs/cockpit", "public"],
    ["https://github.com/orthic-labs/legion", "public"],
    ["https://github.com/bogusyogi/workright", "private"],
    ["https://github.com/orthic-labs/sellright", "public"],
  ]);
  assert.equal(seeded.repositories.some((x) => /rightsuite/.test(x.repositoryIdentity)), false);
  // First install from the default policy yields exactly the committed registry entries.
  const installed = mergeRepositoriesFromInstalledRelease({ ...policy, repositories: [] }, { firstInstall: true });
  assert.deepEqual(installed.trust.repositories, seeded.repositories);
  assert.deepEqual(installed.warnings, []);
});
