import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { MSIX_SIGNING_CONTRACT, validateMsixTarget } from "./msix-config.mjs";
import { generateAppInstaller } from "./msix-appinstaller.mjs";
import { generateAppxManifest } from "./msix-build.mjs";
import { assertPublisherMatchesSigner, planMsixSigning, verifyPayloadInventory } from "./msix-plan.mjs";
import { msixBuildOptions } from "./msix-target.mjs";
import { PROJECT_TYPES, requiredBuildExecutables, resolveProjectType, targetUsesMsix } from "./project-type.mjs";
import { collectPreflight } from "./preflight.mjs";

const publisher = "CN=Damned Ventures LLC, O=Damned Ventures LLC, L=X, S=Y, C=US";
const identity = { name: "Orthic.SwiftRight", publisher, publisherDisplayName: "Orthic Labs", displayName: "SwiftRight", version: "1.2.3.0", architecture: "x64", executable: "SwiftRight.exe" };
const target = () => ({
  signed: true, signingContract: MSIX_SIGNING_CONTRACT, prePackage: { cmd: "node", args: ["stage.mjs"] },
  msix: { stageDir: "stage", out: "dist/SwiftRight.msix", identity: { ...identity }, virtualization: { fileSystemWrite: "disabled", registryWrite: "enabled" },
    assets: { storeLogo: "a.png", square150: "b.png", square44: "c.png" }, runtime: { dlls: ["swiftCore.dll"] },
    appInstaller: { uri: "https://example.com/a.appinstaller", msixUri: "https://example.com/a.msix", out: "dist/a.appinstaller" } },
  installer: { artifacts: [{ file: "dist/SwiftRight.msix", key: "k" }] },
});
const tmp = () => mkdtempSync(path.join(os.tmpdir(), "rk-msix-"));

// Minimal PE builder: MZ + PE32+ + one .idata section listing imports.
function pe(imports = [], machine = 0x8664) {
  const rva = 0x1000, raw = 0x200, section = Buffer.alloc(0x400);
  let cursor = (imports.length + 1) * 20;
  imports.forEach((name, i) => { section.writeUInt32LE(rva + cursor, i * 20 + 12); section.writeUInt32LE(0x2000, i * 20 + 16); section.write(`${name}\0`, cursor, "latin1"); cursor += name.length + 1; });
  const h = Buffer.alloc(raw);
  h.write("MZ", 0, "latin1"); h.writeUInt32LE(0x80, 0x3c); h.writeUInt32LE(0x4550, 0x80);
  h.writeUInt16LE(machine, 0x84); h.writeUInt16LE(1, 0x86); h.writeUInt16LE(240, 0x94); h.writeUInt16LE(0x20b, 0x98);
  h.writeUInt32LE(rva, 0x98 + 120); h.writeUInt32LE((imports.length + 1) * 20, 0x98 + 124);
  const s = 0x98 + 240; h.write(".idata", s, "latin1"); h.writeUInt32LE(0x400, s + 8); h.writeUInt32LE(rva, s + 12); h.writeUInt32LE(0x400, s + 16); h.writeUInt32LE(raw, s + 20);
  return Buffer.concat([h, section]);
}

test("valid msix target passes; every missing explicit field is reported", () => {
  assert.deepEqual(validateMsixTarget(target(), "app"), []);
  const t = target();
  delete t.msix.virtualization.registryWrite; delete t.msix.runtime; t.msix.out = "x.zip"; t.sign = { files: ["a.exe"] }; t.installer = { artifacts: [] };
  const joined = validateMsixTarget(t, "app").join("\n");
  for (const re of [/virtualization\.registryWrite/, /runtime\.dlls/, /\.msix/, /omit sign/, /installer\.artifacts/]) assert.match(joined, re);
});

test("appinstaller updater kind needs https routes; tauri stays default", () => {
  const t = target();
  assert.equal(validateMsixTarget(t, "a").length, 0);
  t.updater = { kind: "appinstaller", artifacts: [] };
  t.msix.appInstaller.uri = "http://x"; 
  assert.match(validateMsixTarget(t, "a").join(), /https/);
  t.updater.kind = "bogus";
  assert.match(validateMsixTarget(t, "a").join(), /updater\.kind/);
});

test("manifest carries explicit virtualization settings and the unvirtualizedResources capability only when disabled", () => {
  const xml = generateAppxManifest({ ...identity, virtualization: { fileSystemWrite: "disabled", registryWrite: "enabled" } });
  assert.match(xml, /<desktop6:FileSystemWriteVirtualization>disabled</);
  assert.match(xml, /<desktop6:RegistryWriteVirtualization>enabled</);
  assert.match(xml, /unvirtualizedResources/);
  const on = generateAppxManifest({ ...identity, virtualization: { fileSystemWrite: "enabled", registryWrite: "enabled" } });
  assert.doesNotMatch(on, /unvirtualizedResources/);
  assert.throws(() => generateAppxManifest({ ...identity, virtualization: { fileSystemWrite: "maybe" } }), { code: "MSIX_INVALID_IDENTITY" });
});

test("signing plan signs DLLs before EXEs, the package after packing, then verifies, appinstaller, seal", () => {
  const plan = planMsixSigning({ files: ["C:/r/App.exe", "C:/r/z.dll", "C:/r/a.dll", "C:/r/AppxManifest.xml"], appInstaller: true });
  assert.deepEqual(plan.map((p) => p.step), ["assemble", "verify-inventory", "sign-pe", "pack", "sign-msix", "verify-msix", "verify-publisher", "appinstaller", "seal"]);
  assert.deepEqual(plan[2].files.map((f) => f.split("/").pop()), ["a.dll", "z.dll", "App.exe"]);
  assert.deepEqual(planMsixSigning({ files: [], sign: false }).map((p) => p.step), ["assemble", "verify-inventory", "pack", "seal"]);
});

test("inventory verification fails closed on missing declared DLL and unresolved imports", () => {
  const root = tmp();
  writeFileSync(path.join(root, "App.exe"), pe(["KERNEL32.dll", "swiftCore.dll", "api-ms-win-crt-heap-l1-1-0.dll"]));
  assert.throws(() => verifyPayloadInventory({ root, inventory: { dlls: ["swiftCore.dll"] }, architecture: "x64" }), (e) => e.code === "MSIX_INVENTORY_INCOMPLETE" && /declared runtime DLL missing/.test(e.message) && /imports swiftCore\.dll/.test(e.message));
  writeFileSync(path.join(root, "swiftCore.dll"), pe(["KERNEL32.dll", "vcruntime140.dll"]));
  assert.throws(() => verifyPayloadInventory({ root, inventory: { dlls: ["swiftCore.dll"] } }), /vcruntime140\.dll/);
  assert.throws(() => verifyPayloadInventory({ root, inventory: { dlls: ["swiftCore.dll"], dependencies: [{ name: "VCLibs", provides: ["vcruntime140.dll"] }] }, architecture: "arm64" }), /is x64, package is arm64/);
  const ok = verifyPayloadInventory({ root, inventory: { dlls: ["swiftCore.dll"], dependencies: [{ name: "VCLibs", provides: ["vcruntime140.dll"] }] }, architecture: "x64" });
  assert.deepEqual(ok.dlls, ["swiftCore.dll"]);
  rmSync(root, { recursive: true });
});

test("signer subject must equal manifest Publisher", () => {
  assertPublisherMatchesSigner(publisher, "O=Damned Ventures LLC, CN=Damned Ventures LLC, L=X, S=Y, C=US");
  assert.throws(() => assertPublisherMatchesSigner(publisher, "CN=Damned Ventures LLC"), { code: "MSIX_PUBLISHER_MISMATCH" });
  assert.throws(() => assertPublisherMatchesSigner(publisher, ""), { code: "MSIX_PUBLISHER_MISMATCH" });
});

test("appinstaller XML pins identity, https URIs and update settings", () => {
  const xml = generateAppInstaller({ uri: "https://e.com/a.appinstaller", msixUri: "https://e.com/a.msix", name: "N", publisher: "CN=P, O=Q", version: "1.0.0.0", architecture: "arm64", hoursBetweenUpdateChecks: 12 });
  assert.match(xml, /MainPackage Name="N" Publisher="CN=P, O=Q" Version="1.0.0.0" ProcessorArchitecture="arm64" Uri="https:\/\/e\.com\/a\.msix"/);
  assert.match(xml, /HoursBetweenUpdateChecks="12" ShowPrompt="false" UpdateBlocksActivation="false"/);
  assert.throws(() => generateAppInstaller({ uri: "http://e.com/a", msixUri: "https://e.com/a.msix", name: "N", publisher: "P", version: "1.0.0.0", architecture: "x64" }), { code: "APPINSTALLER_INVALID" });
});

test("target options resolve against the config root and add appinstaller only for that updater kind", () => {
  const t = target();
  let o = msixBuildOptions({ root: "/r", target: t, app: "a" });
  assert.equal(o.out, path.resolve("/r/dist/SwiftRight.msix"));
  assert.equal(o.appInstaller, undefined);
  assert.equal(o.manifest.virtualization.fileSystemWrite, "disabled");
  t.updater = { kind: "appinstaller", artifacts: [] };
  o = msixBuildOptions({ root: "/r", target: t, app: "a" });
  assert.equal(o.appInstaller.out, path.resolve("/r/dist/a.appinstaller"));
  assert.throws(() => msixBuildOptions({ root: "/r", target: { signingContract: MSIX_SIGNING_CONTRACT }, app: "a" }), { code: "MSIX_CONFIG_INVALID" });
});

test("project type: explicit wins, Package.swift without Cargo is swiftpm, default stays tauri", () => {
  const root = tmp();
  assert.equal(resolveProjectType({ appRoot: root, config: {}, target: {} }), "tauri");
  writeFileSync(path.join(root, "Package.swift"), "");
  assert.equal(resolveProjectType({ appRoot: root, config: {}, target: {} }), "swiftpm");
  mkdirSync(path.join(root, "src-tauri")); writeFileSync(path.join(root, "src-tauri", "Cargo.toml"), "");
  assert.equal(resolveProjectType({ appRoot: root, config: {}, target: {} }), "tauri");
  assert.equal(resolveProjectType({ appRoot: root, config: { projectType: "swiftpm" }, target: {} }), "swiftpm");
  assert.throws(() => resolveProjectType({ config: { projectType: "go" } }), { code: "PROJECT_TYPE_INVALID" });
  assert.ok(PROJECT_TYPES.includes("swiftpm"));
  rmSync(root, { recursive: true });
});

test("toolchain requirements: swift has no cargo/rustc, tauri unchanged", () => {
  assert.deepEqual(requiredBuildExecutables({ projectType: "tauri", packageManager: "pnpm" }), ["git", "pnpm", "cargo", "rustc"]);
  assert.deepEqual(requiredBuildExecutables({ projectType: "swiftpm", usesPackageManager: false }), ["git", "swift"]);
  assert.equal(targetUsesMsix({ msix: {} }), true);
  assert.equal(targetUsesMsix({ signingContract: "windows-raw-exe-authenticode-before-nsis-v1" }), false);
});

const base = { repoRoot: undefined, ownership: false, env: {}, isBrokerManagedHost: () => false };
test("preflight: swift needs swift not sccache/perl/target-bridge; missing swift fails", () => {
  const probes = [];
  const checks = collectPreflight({ ...base, platform: "mac", appRoot: tmp(), projectType: "swiftpm", versionProbe: (c) => { probes.push(c); return c === "swift" ? "swift 6.2" : null; } });
  assert.equal(checks.find((c) => c.name === "swift").status, "ok");
  assert.equal(checks.some((c) => ["target-bridge", "sccache", "compiler-cache"].includes(c.name)), false);
  assert.deepEqual(probes, ["swift"]);
  const bad = collectPreflight({ ...base, platform: "mac", projectType: "swiftpm", versionProbe: () => null });
  assert.equal(bad.find((c) => c.name === "swift").status, "fail");
});

test("preflight: makeappx is required only for msix targets; signtool becomes blocking for msix", () => {
  const none = () => null;
  const plain = collectPreflight({ ...base, platform: "win", findSdkTool: none, versionProbe: none });
  assert.equal(plain.some((c) => c.name === "windows-sdk"), false);
  assert.equal(plain.find((c) => c.name === "signtool").status, "warn");
  const delegate = process.env.MAKEAPPX_PATH; delete process.env.MAKEAPPX_PATH;
  try {
    const m = collectPreflight({ ...base, platform: "win", msix: true, findSdkTool: none, versionProbe: none });
    assert.equal(m.find((c) => c.name === "windows-sdk").status, "fail");
    assert.equal(m.find((c) => c.name === "signtool").status, "fail");
    const found = collectPreflight({ ...base, platform: "win", msix: true, findSdkTool: (t) => ({ version: "10.0.26100.0", toolPath: t }), versionProbe: none });
    assert.equal(found.find((c) => c.name === "windows-sdk").status, "ok");
  } finally { if (delegate !== undefined) process.env.MAKEAPPX_PATH = delegate; }
  const tauri = collectPreflight({ ...base, platform: "linux", appRoot: tmp(), versionProbe: none });
  assert.ok(tauri.some((c) => c.name === "target-bridge"));
});
