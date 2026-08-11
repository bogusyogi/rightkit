import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { addonManifestSha256, addonRoutes, createAddonManifest, validateAddonConfig, validateAddonManifest } from "./addon-contract.mjs";

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-addon-contract-"));
  mkdirSync(path.join(root, "bin")); mkdirSync(path.join(root, "assets")); mkdirSync(path.join(root, "legal"));
  for (const [file, bytes] of [["bin/membrane", "membrane"], ["bin/crypt-service", "service"], ["assets/tab.png", "icon"], ["legal/LICENSE", "license"], ["legal/EULA.txt", "eula"], ["legal/PRIVACY.md", "privacy"], ["legal/THIRD-PARTY-NOTICES.txt", "notice"]]) writeFileSync(path.join(root, file), bytes);
  const files = [
    ["command", "membrane", "bin/membrane", true], ["service", "crypt-service", "bin/crypt-service", true], ["icon", "membrane-tab-icon.png", "assets/tab.png", false],
    ["license", "LICENSE", "legal/LICENSE", false], ["eula", "EULA.txt", "legal/EULA.txt", false], ["privacy", "PRIVACY.md", "legal/PRIVACY.md", false], ["third-party-notices", "THIRD-PARTY-NOTICES.txt", "legal/THIRD-PARTY-NOTICES.txt", false],
  ].map(([role, name, source, executable]) => ({ role, name, source, executable }));
  return { root, config: { schema: 1, kind: "headless-addon", addon: "membrane", version: "0.1.0", packageManager: "pnpm", checks: ["test"], buildInputs: { include: ["right-addon.config.mjs"] }, consumer: { contract: "orthic-product-v1" }, targets: { mac: { targetTriple: "aarch64-apple-darwin", build: { cmd: "cargo", args: ["build"] }, signing: { contract: "apple-developer-id-executable-v1", teamId: "6KLGD3LLKF" }, files }, win: { targetTriple: "x86_64-pc-windows-msvc", build: { cmd: "cargo", args: ["build"] }, signing: { contract: "azure-artifact-signing-v1" }, files } } } };
}

test("add-on manifest has portable immutable identity and routes", () => {
  const { root, config } = fixture();
  const signing = { command: { contract: "test", status: "verified" }, service: { contract: "test", status: "verified" } };
  const manifest = createAddonManifest({ config, root, platform: "mac", commit: "a".repeat(40), signedAt: "2026-08-11T00:00:00.000Z", signing });
  assert.deepEqual(Object.keys(manifest), ["schema", "kind", "addon", "version", "commit", "platform", "targetTriple", "consumer", "files", "signedAt"]);
  const digest = addonManifestSha256(manifest); const routes = addonRoutes(manifest);
  assert.equal(routes.at(-1).manifest, true);
  assert.match(routes[0].key, new RegExp(`^membrane/addons/mac/sha256/${digest}/`));
  assert.ok(routes.slice(0, -1).every((route) => !route.manifest));
});

test("add-on contract rejects unsafe, incomplete, duplicate, and unsigned inputs", () => {
  const { root, config } = fixture();
  const broken = structuredClone(config); broken.targets.mac.files[0].source = "../escape";
  assert.throws(() => validateAddonConfig(broken, { root, platform: "mac" }), /unsafe add-on source/);
  const duplicate = structuredClone(config); duplicate.targets.mac.files[1].role = "command";
  assert.throws(() => validateAddonConfig(duplicate, { root, platform: "mac" }), /duplicate/);
  const signing = { command: { contract: "test", status: "verified" }, service: { contract: "test", status: "verified" } };
  const manifest = createAddonManifest({ config, root, platform: "mac", commit: "b".repeat(40), signing });
  manifest.files.find((file) => file.role === "command").signing = null;
  assert.throws(() => validateAddonManifest(manifest), /unsigned executable/);
});

test("manifest excludes source and mutable transport fields", () => {
  const { root, config } = fixture();
  const manifest = createAddonManifest({ config, root, platform: "win", commit: "c".repeat(40), signing: { command: { contract: "test", status: "verified" }, service: { contract: "test", status: "verified" } } });
  const text = JSON.stringify(manifest);
  assert.doesNotMatch(text, /source|path|url|token/i);
  assert.equal(readFileSync(path.join(root, "bin", "membrane"), "utf8"), "membrane");
});
