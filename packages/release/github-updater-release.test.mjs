import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { digestBytes } from "./publication-artifacts.mjs";
import { createHash, generateKeyPairSync, sign as cryptoSign } from "node:crypto";
import {
  assertUpdaterKeyId,
  assertUpdaterSignature,
  verifyMinisignSignature,
  buildLatestJson,
  githubAssetUrl,
  githubLatestJsonUrl,
  githubTag,
  mergeLatestJson,
  publishGitHubUpdaterRelease,
  resolveUpdaterTrust,
  updaterSignatureKeyId,
} from "./github-updater-release.mjs";
import { RIGHT_SUITE_APP_KEYS, RIGHT_SUITE_UPDATER_PUBLIC_KEYS, RIGHT_SUITE_UPDATER_SIGNING_KEY_ID } from "./suite-api.generated.mjs";

// A Tauri .sig: base64 of a minisign signature file whose second line is base64 of "Ed" + key id (LE) + 64 bytes.
function signatureFor(keyIdHex) {
  const keyId = Buffer.from(keyIdHex, "hex").reverse();
  const raw = Buffer.concat([Buffer.from("Ed"), keyId, Buffer.alloc(64, 7)]);
  const text = `untrusted comment: signature from tauri secret key\n${raw.toString("base64")}\ntrusted comment: t\n${Buffer.alloc(64, 9).toString("base64")}\n`;
  return Buffer.from(text).toString("base64");
}

// A real minisign key whose public text and signatures carry `keyIdHex`, standing in for keys the contract does not list.
function minisignKey(keyIdHex) {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const keyIdLe = Buffer.from(keyIdHex, "hex").reverse();
  const publicKeyText = Buffer.from(`untrusted comment: minisign public key: ${keyIdHex}\n${Buffer.concat([Buffer.from("Ed"), keyIdLe, publicKey.export({ format: "der", type: "spki" }).subarray(-32)]).toString("base64")}\n`).toString("base64");
  const sign = (bytes) => {
    const raw = Buffer.concat([Buffer.from("Ed"), keyIdLe, cryptoSign(null, bytes, privateKey)]);
    const comment = "timestamp:1\tfile:fixture";
    const global = cryptoSign(null, Buffer.concat([raw.subarray(10), Buffer.from(comment)]), privateKey);
    return Buffer.from(`untrusted comment: signature from tauri secret key\n${raw.toString("base64")}\ntrusted comment: ${comment}\n${global.toString("base64")}\n`).toString("base64");
  };
  return { keyId: keyIdHex, publicKeyText, sign };
}

const win = { signature: signatureFor("29395F9FF466261D"), url: "https://github.com/Orthic-Labs/ember/releases/download/v1.0.0/Ember_1.0.0_x64-setup.exe" };
const mac = { signature: signatureFor("29395F9FF466261D"), url: "https://github.com/Orthic-Labs/ember/releases/download/v1.0.0/Ember.app.tar.gz" };

test("latest.json is a Tauri v2 static manifest at the releases/latest URL", () => {
  const latest = buildLatestJson({ version: "1.0.0", notes: "n", pubDate: "2026-10-17T00:00:00.000Z", platforms: { "windows-x86_64": win } });
  assert.deepEqual(Object.keys(latest), ["version", "notes", "pub_date", "platforms"]);
  assert.deepEqual(latest.platforms["windows-x86_64"], win);
  assert.equal(githubLatestJsonUrl("Orthic-Labs/ember"), "https://github.com/Orthic-Labs/ember/releases/latest/download/latest.json");
  assert.equal(githubTag("1.0.0"), "v1.0.0");
  assert.equal(githubAssetUrl("Orthic-Labs/ember", "v1.0.0", "Ember.app.tar.gz"), mac.url);
});

test("latest.json refuses bad platform keys, empty platforms, non-https urls and odd asset names", () => {
  const base = { version: "1.0.0", notes: "", pubDate: "x" };
  assert.throws(() => buildLatestJson({ ...base, platforms: {} }), /at least one platform/);
  assert.throws(() => buildLatestJson({ ...base, platforms: { "windows-x64": win } }), /PUBLICATION_UPDATER_PLATFORM_INVALID/);
  assert.throws(() => buildLatestJson({ ...base, platforms: { "windows-x86_64": { ...win, url: "http://x/y" } } }), /https url/);
  assert.throws(() => githubAssetUrl("Orthic-Labs/ember", "v1.0.0", "Ember Setup.exe"), /PUBLICATION_ASSET_NAME_INVALID/);
  assert.throws(() => githubTag("v1"), /PUBLICATION_VERSION_INVALID/);
});

test("the mac and win runs merge into one latest.json for the same version only", () => {
  const first = buildLatestJson({ version: "1.0.0", notes: "a", pubDate: "1", platforms: { "windows-x86_64": win } });
  const second = buildLatestJson({ version: "1.0.0", notes: "b", pubDate: "2", platforms: { "darwin-aarch64": mac } });
  const merged = mergeLatestJson(first, second);
  assert.deepEqual(Object.keys(merged.platforms), ["darwin-aarch64", "windows-x86_64"]);
  assert.equal(merged.notes, "b");
  assert.equal(mergeLatestJson(null, second), second);
  assert.throws(() => mergeLatestJson({ ...first, version: "0.9.0" }, second), /PUBLICATION_MANIFEST_CONFLICT/);
});

test("signature key id is read from the minisign signature and pinned to the suite key", () => {
  assert.equal(updaterSignatureKeyId(win.signature), "29395F9FF466261D");
  assert.equal(updaterSignatureKeyId("not a signature"), null);
  assertUpdaterKeyId(win.signature, "29395F9FF466261D");
  // No `app`: an undefined id is a no-op only for an app outside the Right Suite (a suite app is enforced below).
  assertUpdaterKeyId(win.signature, undefined);
  assert.throws(() => assertUpdaterKeyId(signatureFor("0123456789ABCDEF"), "29395F9FF466261D"), /PUBLICATION_SIGNATURE_KEY_MISMATCH/);
  assert.throws(() => assertUpdaterKeyId(win.signature, "29395f9ff466261d"), /16 upper-case hex/);
});

test("a suite app resolves the contract signing key by default and needs no declared key", () => {
  const signing = RIGHT_SUITE_UPDATER_PUBLIC_KEYS.find((key) => key.kid === RIGHT_SUITE_UPDATER_SIGNING_KEY_ID);
  assert.ok(signing, "the contract lists its own signing key");
  for (const app of RIGHT_SUITE_APP_KEYS) {
    assert.deepEqual(resolveUpdaterTrust({ app }), { keyId: RIGHT_SUITE_UPDATER_SIGNING_KEY_ID, publicKey: signing.publicKey, suite: true }, app);
  }
});

test("a suite app may name any unexpired contract key id and gets that key's text", () => {
  const current = minisignKey("1111111111111111");
  const next = minisignKey("2222222222222222");
  const keys = [{ kid: current.keyId, publicKey: current.publicKeyText, notAfter: null, notAfterUnix: null }, { kid: next.keyId, publicKey: next.publicKeyText, notAfter: null, notAfterUnix: null }];
  assert.deepEqual(resolveUpdaterTrust({ app: "mailright", configuredKeyId: next.keyId, keys }), { keyId: next.keyId, publicKey: next.publicKeyText, suite: true });
  assert.deepEqual(resolveUpdaterTrust({ app: "mailright", configuredKeyId: RIGHT_SUITE_UPDATER_SIGNING_KEY_ID }).keyId, RIGHT_SUITE_UPDATER_SIGNING_KEY_ID);
  // The default is the contract's signing key, so a list that does not hold it leaves no default to resolve.
  assert.throws(() => resolveUpdaterTrust({ app: "mailright", keys }), /PUBLICATION_SIGNATURE_KEY_MISMATCH.*not an updater key in the Right Suite contract/);
});

test("a suite app refuses a key id the contract does not list, however it is spelled", () => {
  for (const configuredKeyId of ["0123456789ABCDEF", RIGHT_SUITE_UPDATER_SIGNING_KEY_ID.toLowerCase(), "29395F9FF466261", "", 123, false]) {
    assert.throws(() => resolveUpdaterTrust({ app: "pulse", configuredKeyId }), (error) => error.code === "PUBLICATION_SIGNATURE_KEY_MISMATCH" && /is not an updater key in the Right Suite contract/.test(error.message), String(configuredKeyId));
  }
});

test("a contract key is refused from the first second its notAfterUnix names", () => {
  const retired = minisignKey("3333333333333333");
  const notAfterUnix = Date.parse("2027-01-01T00:00:00Z") / 1000;
  const keys = [{ kid: retired.keyId, publicKey: retired.publicKeyText, notAfter: "2026-12-31", notAfterUnix }];
  const trust = resolveUpdaterTrust({ app: "viewright", configuredKeyId: retired.keyId, keys, now: notAfterUnix - 1 });
  assert.equal(trust.publicKey, retired.publicKeyText);
  for (const now of [notAfterUnix, notAfterUnix + 86400]) {
    assert.throws(() => resolveUpdaterTrust({ app: "viewright", configuredKeyId: retired.keyId, keys, now }), (error) => error.code === "PUBLICATION_SIGNATURE_KEY_MISMATCH" && /ended \(notAfter 2026-12-31\)/.test(error.message));
  }
  // `now` defaults to the clock: a key that ended long ago is refused without injecting it.
  const expired = [{ kid: retired.keyId, publicKey: retired.publicKeyText, notAfter: "2000-01-01", notAfterUnix: Date.parse("2000-01-02T00:00:00Z") / 1000 }];
  assert.throws(() => resolveUpdaterTrust({ app: "viewright", configuredKeyId: retired.keyId, keys: expired }), /no longer accepted/);
});

test("a suite app may repeat the contract key text but never declare another key", () => {
  const signing = RIGHT_SUITE_UPDATER_PUBLIC_KEYS.find((key) => key.kid === RIGHT_SUITE_UPDATER_SIGNING_KEY_ID);
  for (const configuredPublicKey of [undefined, null, "", "  \n", signing.publicKey]) {
    assert.equal(resolveUpdaterTrust({ app: "coderight", configuredPublicKey }).publicKey, signing.publicKey, JSON.stringify(configuredPublicKey));
  }
  const foreign = minisignKey("4444444444444444").publicKeyText;
  for (const configuredPublicKey of [foreign, `${signing.publicKey}\n`, ` ${signing.publicKey}`, signing.publicKey.slice(0, -4), 123, {}]) {
    assert.throws(() => resolveUpdaterTrust({ app: "coderight", configuredPublicKey }), (error) => error.code === "PUBLICATION_SIGNATURE_KEY_MISMATCH" && /not the Right Suite contract key for 29395F9FF466261D; suite apps do not declare their own/.test(error.message));
  }
  // The declared key is judged against the key the id selects, not against every listed key.
  const other = minisignKey("5555555555555555");
  const keys = [{ kid: signing.kid, publicKey: signing.publicKey, notAfter: null, notAfterUnix: null }, { kid: other.keyId, publicKey: other.publicKeyText, notAfter: null, notAfterUnix: null }];
  assert.throws(() => resolveUpdaterTrust({ app: "coderight", configuredPublicKey: other.publicKeyText, configuredKeyId: signing.kid, keys }), /suite apps do not declare their own/);
  assert.equal(resolveUpdaterTrust({ app: "coderight", configuredPublicKey: other.publicKeyText, configuredKeyId: other.keyId, keys }).publicKey, other.publicKeyText);
});

test("an app outside the suite keeps its declared key and an optional key id", () => {
  const ember = minisignKey("6666666666666666");
  assert.deepEqual(resolveUpdaterTrust({ app: "ember", configuredPublicKey: ember.publicKeyText }), { keyId: undefined, publicKey: ember.publicKeyText, suite: false });
  assert.deepEqual(resolveUpdaterTrust({ app: "ember", configuredPublicKey: ember.publicKeyText, configuredKeyId: ember.keyId }), { keyId: ember.keyId, publicKey: ember.publicKeyText, suite: false });
  // The contract's keys and clock play no part: an id the contract does not list is the app's own business.
  assert.equal(resolveUpdaterTrust({ app: "ember", configuredPublicKey: ember.publicKeyText, configuredKeyId: ember.keyId, keys: [], now: Number.MAX_SAFE_INTEGER }).keyId, ember.keyId);
  assert.equal(resolveUpdaterTrust({ app: undefined, configuredPublicKey: ember.publicKeyText }).suite, false);
});

test("a suite app is a suite app in any spelling of its name: the contract stays the trust root", () => {
  for (const app of ["Mailright", "MAILRIGHT"]) {
    assert.equal(resolveUpdaterTrust({ app }).suite, true, app);
    assert.throws(() => resolveUpdaterTrust({ app, configuredPublicKey: "RWQnotthecontractkey" }), /not the Right Suite contract key/, app);
    assert.throws(() => assertUpdaterKeyId(signatureFor("0123456789ABCDEF"), undefined, "x", { app }), /was not made by updater key/, app);
  }
});

test("an app outside the suite with no declared key is unverifiable, never silently trusted", () => {
  for (const configuredPublicKey of [undefined, null, "", " \t\n", 123]) {
    assert.throws(() => resolveUpdaterTrust({ app: "ember", configuredPublicKey }), (error) => error.code === "PUBLICATION_SIGNATURE_UNVERIFIABLE" && /updater public key is required/.test(error.message), JSON.stringify(configuredPublicKey));
  }
});

test("a suite app's key id is enforced even when the caller passes none", () => {
  const bySigningKey = signatureFor(RIGHT_SUITE_UPDATER_SIGNING_KEY_ID);
  assertUpdaterKeyId(bySigningKey, undefined, "x", { app: "genright" });
  assertUpdaterKeyId(bySigningKey, RIGHT_SUITE_UPDATER_SIGNING_KEY_ID, "x", { app: "genright" });
  assert.throws(() => assertUpdaterKeyId(signatureFor("0123456789ABCDEF"), undefined, "the sig", { app: "genright" }), /PUBLICATION_SIGNATURE_KEY_MISMATCH.*the sig was not made by updater key 29395F9FF466261D \(found 0123456789ABCDEF\)/);
  assert.throws(() => assertUpdaterKeyId("not a signature", undefined, "x", { app: "genright" }), /found unreadable/);
  // An id the contract does not accept is refused for a suite app even when the signature carries it.
  assert.throws(() => assertUpdaterKeyId(signatureFor("0123456789ABCDEF"), "0123456789ABCDEF", "x", { app: "genright" }), /not an updater key in the Right Suite contract/);
  // Outside the suite an undefined id stays a no-op and an explicit one is pinned, with or without `app`.
  assertUpdaterKeyId(signatureFor("0123456789ABCDEF"), undefined, "x", { app: "ember" });
  assertUpdaterKeyId(signatureFor("0123456789ABCDEF"), "0123456789ABCDEF", "x", { app: "ember" });
  assert.throws(() => assertUpdaterKeyId(bySigningKey, "0123456789ABCDEF", "x", { app: "ember" }), /PUBLICATION_SIGNATURE_KEY_MISMATCH/);
  assert.throws(() => assertUpdaterKeyId(bySigningKey, "29395f9ff466261d", "x", { app: "ember" }), /16 upper-case hex/);
});

test("trust resolved for a suite app accepts its key's signature and refuses another listed key's", () => {
  const current = minisignKey("7777777777777777");
  const rotated = minisignKey("8888888888888888");
  const keys = [{ kid: current.keyId, publicKey: current.publicKeyText, notAfter: null, notAfterUnix: null }, { kid: rotated.keyId, publicKey: rotated.publicKeyText, notAfter: null, notAfterUnix: null }];
  const bytes = Buffer.from("final artifact bytes");
  const trust = resolveUpdaterTrust({ app: "postright", configuredKeyId: current.keyId, keys });
  const own = current.sign(bytes);
  assertUpdaterKeyId(own, trust.keyId, "sig", { app: "postright", keys });
  assertUpdaterSignature(bytes, own, trust.publicKey);
  // A signature by the other listed key is a valid signature, but not the one this release is pinned to.
  const foreign = rotated.sign(bytes);
  assert.equal(verifyMinisignSignature(bytes, foreign, rotated.publicKeyText), true);
  assert.throws(() => assertUpdaterKeyId(foreign, trust.keyId, "sig", { app: "postright", keys }), /PUBLICATION_SIGNATURE_KEY_MISMATCH.*not made by updater key 7777777777777777 \(found 8888888888888888\)/);
  assert.throws(() => assertUpdaterSignature(bytes, foreign, trust.publicKey), /PUBLICATION_SIGNATURE_INVALID/);
});

test("minisign signatures verify cryptographically over the exact bytes (Ed and ED prehash)", () => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const rawKey = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  const keyIdHex = "29395F9FF466261D";
  const keyIdLe = Buffer.from(keyIdHex, "hex").reverse();
  const pubText = Buffer.from(`untrusted comment: minisign public key: ${keyIdHex}\n${Buffer.concat([Buffer.from("Ed"), keyIdLe, rawKey]).toString("base64")}\n`).toString("base64");
  const makeSig = (algorithm, bytes, key = privateKey) => {
    const data = algorithm === "ED" ? createHash("blake2b512").update(bytes).digest() : bytes;
    const raw = Buffer.concat([Buffer.from(algorithm), keyIdLe, cryptoSign(null, data, key)]);
    const comment = "timestamp:1\tfile:fixture\tcomment: signed ✓  ";
    const global = cryptoSign(null, Buffer.concat([raw.subarray(10), Buffer.from(comment)]), key);
    return Buffer.from(`untrusted comment: signature from tauri secret key\n${raw.toString("base64")}\ntrusted comment: ${comment}\n${global.toString("base64")}\n`).toString("base64");
  };
  const bytes = Buffer.from("final artifact bytes");
  for (const algorithm of ["Ed", "ED"]) {
    const sig = makeSig(algorithm, bytes);
    assert.equal(verifyMinisignSignature(bytes, sig, pubText), true, algorithm);
    assertUpdaterSignature(bytes, sig, pubText);
    assert.equal(verifyMinisignSignature(Buffer.from("tampered bytes"), sig, pubText), false, `${algorithm} tampered`);
    assert.throws(() => assertUpdaterSignature(Buffer.from("tampered bytes"), sig, pubText), /PUBLICATION_SIGNATURE_INVALID/);
    const lines = Buffer.from(sig, "base64").toString("utf8").trimEnd().split("\n");
    const encode = (value) => Buffer.from(`${value.join("\n")}\n`).toString("base64");
    const rejected = [
      lines.slice(0, 3),
      [lines[0], lines[1], lines[2], Buffer.alloc(64).toString("base64")],
      [lines[0], lines[1], lines[2].replace("timestamp:1", "timestamp:2"), lines[3]],
      [lines[0], lines[1], "comment: invalid prefix", lines[3]],
      [lines[0], lines[1], lines[2], "invalid!"],
      [lines[0], lines[1], lines[2], Buffer.alloc(63).toString("base64")],
      [...lines, "extra field"],
    ];
    for (const invalid of rejected) assert.equal(verifyMinisignSignature(bytes, encode(invalid), pubText), false, `${algorithm} invalid envelope/global`);
    assert.equal(verifyMinisignSignature(bytes, Buffer.from(`${lines.join("\r\n")}\r\n`).toString("base64"), pubText), true, `${algorithm} CRLF`);
  }
  const other = generateKeyPairSync("ed25519");
  assert.equal(verifyMinisignSignature(bytes, makeSig("Ed", bytes, other.privateKey), pubText), false, "wrong key");
  assert.equal(verifyMinisignSignature(bytes, "not a signature", pubText), false, "malformed signature");
  assert.equal(verifyMinisignSignature(bytes, makeSig("Ed", bytes), "not a key"), false, "malformed key");
  assert.equal(verifyMinisignSignature(bytes, makeSig("Ed", bytes), undefined), false, "missing key");
});

// Fake gh: a release with assets held in memory.
function fakeGh({ releaseExists = false, remoteLatest = null } = {}) {
  const calls = [];
  const remote = new Map();
  if (remoteLatest) remote.set("latest.json", Buffer.from(JSON.stringify(remoteLatest)));
  let exists = releaseExists;
  const run = (command, args, { allowFailure = false } = {}) => {
    calls.push(args.slice(0, 2).join(" "));
    const ok = (output = "") => (allowFailure ? { ok: true, output } : output);
    const fail = (error) => {
      if (allowFailure) return { ok: false, output: "", error };
      throw new Error(error);
    };
    if (args[0] === "release" && args[1] === "view") return exists ? ok("{}") : fail("release not found");
    if (args[0] === "release" && args[1] === "create") { exists = true; return ok(); }
    if (args[0] === "release" && args[1] === "upload") {
      const tagIndex = 2;
      for (const file of args.slice(tagIndex + 1).filter((value) => !value.startsWith("--") && value !== args[args.indexOf("--repo") + 1])) remote.set(path.basename(file), readFileSync(file));
      return ok();
    }
    if (args[0] === "release" && args[1] === "download") {
      const name = args[args.indexOf("--pattern") + 1];
      if (!remote.has(name)) return fail("no assets match the file pattern");
      writeFileSync(path.join(args[args.indexOf("--dir") + 1], name), remote.get(name));
      return ok();
    }
    throw new Error(`unexpected gh call: ${args.join(" ")}`);
  };
  return { run, calls, remote };
}

function sealedAsset(dir, name, content) {
  const file = path.join(dir, name);
  writeFileSync(file, content);
  return { file, evidence: { file, sha256: digestBytes(Buffer.from(content)), sizeBytes: Buffer.byteLength(content) } };
}

test("publishes bundles, signatures and latest.json, creating the release once", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-gh-updater-"));
  const gh = fakeGh();
  const latest = buildLatestJson({ version: "1.0.0", notes: "n", pubDate: "1", platforms: { "windows-x86_64": win } });
  const result = publishGitHubUpdaterRelease({
    repo: "Orthic-Labs/ember",
    tag: "v1.0.0",
    title: "ember 1.0.0",
    notes: "n",
    assets: [sealedAsset(dir, "Ember_1.0.0_x64-setup.exe", "bundle"), sealedAsset(dir, "Ember_1.0.0_x64-setup.exe.sig", win.signature)],
    latest,
    stateDir: path.join(dir, "state"),
    run: gh.run,
  });
  assert.equal(result.status, "created");
  assert.equal(result.latestJsonUrl, "https://github.com/Orthic-Labs/ember/releases/latest/download/latest.json");
  assert.deepEqual([...gh.remote.keys()].sort(), ["Ember_1.0.0_x64-setup.exe", "Ember_1.0.0_x64-setup.exe.sig", "latest.json"]);
  assert.equal(JSON.parse(gh.remote.get("latest.json")).platforms["windows-x86_64"].url, win.url);
  assert.equal(gh.calls.filter((call) => call === "release create").length, 1);
  assert.ok(!existsSync(path.join(dir, "state", "release-notes.md")));
});

test("a second platform run keeps the first platform's entry in latest.json", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-gh-updater-"));
  const existing = buildLatestJson({ version: "1.0.0", notes: "n", pubDate: "1", platforms: { "windows-x86_64": win } });
  const gh = fakeGh({ releaseExists: true, remoteLatest: existing });
  const latest = buildLatestJson({ version: "1.0.0", notes: "n", pubDate: "2", platforms: { "darwin-aarch64": mac } });
  const result = publishGitHubUpdaterRelease({
    repo: "Orthic-Labs/ember",
    tag: "v1.0.0",
    title: "t",
    notes: "n",
    assets: [sealedAsset(dir, "Ember.app.tar.gz", "mac bundle")],
    latest,
    stateDir: path.join(dir, "state"),
    run: gh.run,
  });
  assert.equal(result.status, "updated");
  assert.deepEqual(Object.keys(JSON.parse(gh.remote.get("latest.json")).platforms), ["darwin-aarch64", "windows-x86_64"]);
  assert.equal(gh.calls.filter((call) => call === "release create").length, 0);
});

test("a remote asset with different bytes is refused, never overwritten", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-gh-updater-"));
  const gh = fakeGh({ releaseExists: true });
  gh.remote.set("Ember.app.tar.gz", Buffer.from("different"));
  const latest = buildLatestJson({ version: "1.0.0", notes: "", pubDate: "1", platforms: { "darwin-aarch64": mac } });
  assert.throws(() => publishGitHubUpdaterRelease({
    repo: "Orthic-Labs/ember", tag: "v1.0.0", title: "t", notes: "n",
    assets: [sealedAsset(dir, "Ember.app.tar.gz", "mac bundle")], latest, stateDir: path.join(dir, "state"), run: gh.run,
  }), /hash mismatch/);
});

test("a dry run makes no gh call", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-gh-updater-"));
  const gh = fakeGh();
  const latest = buildLatestJson({ version: "1.0.0", notes: "", pubDate: "1", platforms: { "darwin-aarch64": mac } });
  const result = publishGitHubUpdaterRelease({
    repo: "Orthic-Labs/ember", tag: "v1.0.0", title: "t", notes: "n",
    assets: [{ file: path.join(dir, "Ember.app.tar.gz") }], latest, stateDir: path.join(dir, "state"), dryRun: true, run: gh.run,
  });
  assert.equal(result.status, "would-publish");
  assert.equal(gh.calls.length, 0);
});

// The same vectors rightkit-license (Rust) and @rightkit/app-update (TypeScript) read a signature's key id against, so the
// three readers cannot drift apart. contracts/ holds the original; crates/rightkit-license/test-vectors/ a byte copy.
test("updaterSignatureKeyId reads exactly what the shared signature vectors say", () => {
  const vectors = JSON.parse(readFileSync(new URL("../../contracts/updater-signature-vectors.v1.json", import.meta.url), "utf8"));
  assert.equal(vectors.schema, "updater-signature-key-id.v1");
  assert.ok(vectors.cases.length >= 15);
  for (const vector of vectors.cases) assert.equal(updaterSignatureKeyId(vector.signature), vector.keyId, vector.name);
});
