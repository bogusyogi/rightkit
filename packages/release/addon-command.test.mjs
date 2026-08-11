import assert from "node:assert/strict";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import { createAddonManifest, addonManifestSha256, canonicalAddonManifest, downloadImmutableAddOn } from "./addon-contract.mjs";

const command = fileURLToPath(new URL("./addon-command.mjs", import.meta.url));
function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-addon-command-"));
  mkdirSync(path.join(root, "legal")); mkdirSync(path.join(root, "assets"));
  for (const name of ["LICENSE", "EULA.txt", "PRIVACY.md", "THIRD-PARTY-NOTICES.txt"]) writeFileSync(path.join(root, "legal", name), name);
  writeFileSync(path.join(root, "assets", "tab.png"), "icon");
  const files = [["command", "membrane", "out/membrane", true], ["service", "crypt-service", "out/crypt-service", true], ["icon", "membrane-tab-icon.png", "assets/tab.png", false], ["license", "LICENSE", "legal/LICENSE", false], ["eula", "EULA.txt", "legal/EULA.txt", false], ["privacy", "PRIVACY.md", "legal/PRIVACY.md", false], ["third-party-notices", "THIRD-PARTY-NOTICES.txt", "legal/THIRD-PARTY-NOTICES.txt", false]].map(([role, name, source, executable]) => ({ role, name, source, executable }));
  writeFileSync(path.join(root, "right-addon.config.mjs"), `export default ${JSON.stringify({ schema: 1, kind: "headless-addon", addon: "membrane", version: "0.1.0", packageManager: "pnpm", checks: [], buildInputs: { include: ["right-addon.config.mjs"] }, consumer: { contract: "orthic-product-v1" }, targets: { mac: { targetTriple: "aarch64-apple-darwin", build: { cmd: "false", args: [] }, signing: { contract: "apple-developer-id-executable-v1", teamId: "6KLGD3LLKF" }, files }, win: { targetTriple: "x86_64-pc-windows-msvc", build: { cmd: "false", args: [] }, signing: { contract: "azure-artifact-signing-v1" }, files } } }, null, 2)};\n`);
  execFileSync("git", ["init", "--initial-branch", "main"], { cwd: root }); execFileSync("git", ["config", "user.email", "test@example.com"], { cwd: root }); execFileSync("git", ["config", "user.name", "Test"], { cwd: root }); execFileSync("git", ["add", "."], { cwd: root }); execFileSync("git", ["commit", "-m", "init"], { cwd: root });
  return root;
}
test("add-on dry-run creates no release state and never executes build", () => {
  const root = fixture(); const result = spawnSync(process.execPath, [command, "build", "--config", "right-addon.config.mjs", "--platform", "mac", "--dry-run"], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr); assert.match(result.stdout, /would run: false/); assert.equal(existsSync(path.join(root, ".right-release")), false);
});

test("local adoption verifies lock and stages portable plus Tauri-qualified binaries", async () => {
  const root = fixture(); const config = (await import(`${pathToFileURL(path.join(root, "right-addon.config.mjs")).href}?test=${Date.now()}`)).default;
  mkdirSync(path.join(root, "out")); writeFileSync(path.join(root, "out", "membrane"), "command"); writeFileSync(path.join(root, "out", "crypt-service"), "service");
  const manifest = createAddonManifest({ config, root, platform: "mac", commit: "d".repeat(40), signing: { command: { contract: "test-fixture-v1", status: "verified" }, service: { contract: "test-fixture-v1", status: "verified" } } });
  const sealed = path.join(root, "sealed"); mkdirSync(sealed);
  for (const file of manifest.files) writeFileSync(path.join(sealed, file.name), readFileSync(path.join(root, config.targets.mac.files.find((entry) => entry.role === file.role).source)));
  writeFileSync(path.join(sealed, "addon-manifest.json"), canonicalAddonManifest(manifest));
  const digest = addonManifestSha256(manifest); const lock = path.join(root, "membrane.lock.json"); writeFileSync(lock, JSON.stringify({ targets: { mac: { manifestUrl: `https://example.test/membrane/addons/mac/sha256/${digest}/addon-manifest.json`, manifestSha256: digest } } }));
  const output = path.join(root, "addons"); const unsigned = spawnSync(process.execPath, [command, "adopt", "--platform", "mac", "--lock", lock, "--output", `${output}-unsigned`, "--source", sealed], { cwd: root, encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_TEST_FIXTURE: "" } });
  assert.notEqual(unsigned.status, 0); assert.match(unsigned.stderr, /unexpected mac signing contract/);
  const result = spawnSync(process.execPath, [command, "adopt", "--platform", "mac", "--lock", lock, "--output", output, "--source", sealed], { cwd: root, encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_TEST_FIXTURE: "1" } });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(path.join(output, "membrane", "bin", "membrane-aarch64-apple-darwin"), "utf8"), "command");
  assert.equal(readFileSync(path.join(output, "membrane", "resources", "LICENSE"), "utf8"), "LICENSE");
  const receipt = JSON.parse(readFileSync(path.join(root, ".right-release", "addons", "adoptions", "membrane-mac.json"), "utf8"));
  assert.equal(receipt.manifestSha256, digest);
  assert.deepEqual(receipt.files.map((file) => [file.role, file.sealedSha256, file.adoptedSha256]), [["command", manifest.files.find((file) => file.role === "command").sha256, manifest.files.find((file) => file.role === "command").sha256], ["service", manifest.files.find((file) => file.role === "service").sha256, manifest.files.find((file) => file.role === "service").sha256]]);
});

test("remote downloader accepts only immutable manifest plus file routes", async () => {
  const root = fixture(); const config = (await import(`${pathToFileURL(path.join(root, "right-addon.config.mjs")).href}?remote=${Date.now()}`)).default;
  mkdirSync(path.join(root, "out")); writeFileSync(path.join(root, "out", "membrane"), "command"); writeFileSync(path.join(root, "out", "crypt-service"), "service");
  const manifest = createAddonManifest({ config, root, platform: "mac", commit: "e".repeat(40), signing: { command: { contract: "test-fixture-v1", status: "verified" }, service: { contract: "test-fixture-v1", status: "verified" } } });
  const digest = addonManifestSha256(manifest); const base = `https://example.test/membrane/addons/mac/sha256/${digest}/`; const bytes = new Map([[`${base}addon-manifest.json`, Buffer.from(canonicalAddonManifest(manifest))], ...manifest.files.map((file) => [base + encodeURIComponent(file.name), readFileSync(path.join(root, config.targets.mac.files.find((entry) => entry.role === file.role).source))])]);
  const destination = path.join(root, "remote"); mkdirSync(destination);
  await downloadImmutableAddOn(`${base}addon-manifest.json`, destination, { fetchImpl: async (url) => ({ ok: bytes.has(url), status: bytes.has(url) ? 200 : 404, arrayBuffer: async () => bytes.get(url) }) });
  assert.equal(readFileSync(path.join(destination, "crypt-service"), "utf8"), "service");
});
