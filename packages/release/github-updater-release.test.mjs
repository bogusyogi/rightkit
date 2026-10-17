import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { digestBytes } from "./publication-artifacts.mjs";
import {
  assertUpdaterKeyId,
  buildLatestJson,
  githubAssetUrl,
  githubLatestJsonUrl,
  githubTag,
  mergeLatestJson,
  publishGitHubUpdaterRelease,
  updaterSignatureKeyId,
} from "./github-updater-release.mjs";

// A Tauri .sig: base64 of a minisign signature file whose second line is base64 of "Ed" + key id (LE) + 64 bytes.
function signatureFor(keyIdHex) {
  const keyId = Buffer.from(keyIdHex, "hex").reverse();
  const raw = Buffer.concat([Buffer.from("Ed"), keyId, Buffer.alloc(64, 7)]);
  const text = `untrusted comment: signature from tauri secret key\n${raw.toString("base64")}\ntrusted comment: t\n${Buffer.alloc(64, 9).toString("base64")}\n`;
  return Buffer.from(text).toString("base64");
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
  assertUpdaterKeyId(win.signature, undefined);
  assert.throws(() => assertUpdaterKeyId(signatureFor("0123456789ABCDEF"), "29395F9FF466261D"), /PUBLICATION_SIGNATURE_KEY_MISMATCH/);
  assert.throws(() => assertUpdaterKeyId(win.signature, "29395f9ff466261d"), /16 upper-case hex/);
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
