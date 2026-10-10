import test from "node:test";
import assert from "node:assert/strict";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { readPeImports } from "./pe-imports.mjs";
import { stageSwiftWindows } from "./swift-windows-stage.mjs";
import { assembleMsixRoot, buildMsix, generateAppxManifest, peFilesIn } from "./msix-build.mjs";
import { assembleMacApp, discoverNestedCode, generateInfoPlist, signAndNotarizeMacApp } from "./swift-mac-app.mjs";
import { loadSwiftPackageConfig, macOptions, main, msixOptions, windowsStageOptions } from "./swift-package-command.mjs";

// Build a tiny valid PE32+ with the given import names (one .idata section).
function pe({ imports = [], delay = [], machine = 0x8664, subsystem = 3 } = {}) {
  const rva = 0x1000, raw = 0x200;
  const section = Buffer.alloc(0x400);
  let cursor = 0;
  const tableSize = (imports.length + 1) * 20, delaySize = (delay.length + 1) * 32;
  const importAt = cursor; cursor += tableSize;
  const delayAt = cursor; cursor += delaySize;
  imports.forEach((name, i) => {
    const nameRva = rva + cursor; section.write(`${name}\0`, cursor, "latin1"); cursor += name.length + 1;
    section.writeUInt32LE(nameRva, importAt + i * 20 + 12);
    section.writeUInt32LE(0x2000, importAt + i * 20 + 16);
  });
  delay.forEach((name, i) => {
    const nameRva = rva + cursor; section.write(`${name}\0`, cursor, "latin1"); cursor += name.length + 1;
    section.writeUInt32LE(1, delayAt + i * 32); section.writeUInt32LE(nameRva, delayAt + i * 32 + 4);
  });
  const header = Buffer.alloc(raw);
  header.write("MZ", 0, "latin1"); header.writeUInt32LE(0x80, 0x3c);
  header.writeUInt32LE(0x00004550, 0x80);
  header.writeUInt16LE(machine, 0x84); header.writeUInt16LE(1, 0x86); header.writeUInt16LE(240, 0x94);
  const opt = 0x98; header.writeUInt16LE(0x20b, opt);
  header.writeUInt16LE(subsystem, opt + 68);
  header.writeUInt32LE(rva, opt + 112 + 8); header.writeUInt32LE(tableSize, opt + 112 + 12);
  if (delay.length) { header.writeUInt32LE(rva + delayAt, opt + 112 + 13 * 8); header.writeUInt32LE(delaySize, opt + 112 + 13 * 8 + 4); }
  const sec = opt + 240;
  header.write(".idata", sec, "latin1"); header.writeUInt32LE(0x400, sec + 8); header.writeUInt32LE(rva, sec + 12);
  header.writeUInt32LE(0x400, sec + 16); header.writeUInt32LE(raw, sec + 20);
  return Buffer.concat([header, section]);
}

const tmp = () => mkdtempSync(path.join(os.tmpdir(), "rk-swiftpkg-"));
const write = (dir, name, data) => { const f = path.join(dir, name); mkdirSync(path.dirname(f), { recursive: true }); writeFileSync(f, data); return f; };

test("PE reader lists import and delay-load DLLs and architecture", () => {
  const info = readPeImports(pe({ imports: ["KERNEL32.dll", "swiftCore.dll"], delay: ["swiftright_render.dll"], machine: 0xaa64 }));
  assert.deepEqual(info.imports, ["KERNEL32.dll", "swiftCore.dll"]);
  assert.deepEqual(info.delayImports, ["swiftright_render.dll"]);
  assert.equal(info.architecture, "arm64");
  assert.throws(() => readPeImports(Buffer.from("nope")), { code: "PE_INVALID" });
});

test("stage resolves transitive app-local DLLs from search dirs and records hashes", () => {
  const root = tmp();
  const exe = write(root, "build/App.exe", pe({ imports: ["KERNEL32.dll", "api-ms-win-crt-runtime-l1-1-0.dll", "swiftCore.dll"], delay: ["swiftright_render.dll"] }));
  write(root, "swift/swiftCore.dll", pe({ imports: ["KERNEL32.dll", "vcruntime140.dll"] }));
  write(root, "swift/vcruntime140.dll", pe({ imports: ["KERNEL32.dll"] }));
  write(root, "render/swiftright_render.dll", pe({ imports: ["d3d12.dll", "swiftCore.dll"] }));
  const out = path.join(root, "stage");
  const m = stageSwiftWindows({ exe, out, searchDirs: [path.join(root, "swift"), path.join(root, "render")] });
  assert.deepEqual(m.files.map((f) => f.name), ["App.exe", "swiftCore.dll", "swiftright_render.dll", "vcruntime140.dll"]);
  assert.equal(m.architecture, "x64");
  assert.ok(m.systemDlls.includes("api-ms-win-crt-runtime-l1-1-0.dll"));
  assert.ok(existsSync(path.join(out, "swiftright_render.dll")));
  assert.ok(existsSync(`${out}.stage.json`));
  assert.ok(!existsSync(path.join(out, ".stage-manifest.json")));
  rmSync(root, { recursive: true });
});

test("stage fails closed on unresolved dependency, naming importer", () => {
  const root = tmp();
  const exe = write(root, "App.exe", pe({ imports: ["swiftCore.dll", "KERNEL32.dll"] }));
  write(root, "swift/swiftCore.dll", pe({ imports: ["msvcp140.dll"] }));
  assert.throws(() => stageSwiftWindows({ exe, out: path.join(root, "s"), searchDirs: [path.join(root, "swift")] }),
    (e) => e.code === "SWIFT_STAGE_UNRESOLVED" && /msvcp140\.dll \(needed by swiftCore\.dll\)/.test(e.message));
  // a declared package dependency satisfies it
  const m = stageSwiftWindows({ exe, out: path.join(root, "s"), searchDirs: [path.join(root, "swift")],
    dependencies: [{ name: "Microsoft.VCLibs.140.00.UWPDesktop", provides: ["msvcp140.dll"] }] });
  assert.equal(m.providedByDependencies["msvcp140.dll"], "Microsoft.VCLibs.140.00.UWPDesktop");
  rmSync(root, { recursive: true });
});

test("stage enforces the declared Windows subsystem on the root exe only", () => {
  const dir = tmp();
  try {
    const consoleExe = write(dir, "bin/App.exe", pe({ imports: ["KERNEL32.dll"] }));
    assert.equal(readPeImports(readFileSync(consoleExe)).subsystem, 3);
    assert.throws(() => stageSwiftWindows({ exe: consoleExe, out: path.join(dir, "out"), subsystem: "gui" }),
      (e) => e.code === "SWIFT_STAGE_SUBSYSTEM" && /\/SUBSYSTEM:WINDOWS/.test(e.message) && /ENTRY:mainCRTStartup/.test(e.message));
    assert.throws(() => stageSwiftWindows({ exe: consoleExe, out: path.join(dir, "out"), subsystem: "windowed" }), { code: "SWIFT_STAGE_OPTIONS" });
    const guiExe = write(dir, "gui/App.exe", pe({ imports: ["KERNEL32.dll"], subsystem: 2 }));
    assert.equal(stageSwiftWindows({ exe: guiExe, out: path.join(dir, "out2"), subsystem: "gui" }).architecture, "x64");
    assert.ok(stageSwiftWindows({ exe: consoleExe, out: path.join(dir, "out3") }));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("stage rejects mixed architectures", () => {
  const root = tmp();
  const exe = write(root, "App.exe", pe({ imports: ["x.dll"] }));
  write(root, "x.dll", pe({ machine: 0xaa64 }));
  assert.throws(() => stageSwiftWindows({ exe, out: path.join(root, "s") }), { code: "SWIFT_STAGE_ARCH" });
  rmSync(root, { recursive: true });
});

const identity = { name: "Orthic.SwiftRight", version: "0.1.0.0", architecture: "x64", publisher: "CN=Damned Ventures LLC, O=Damned Ventures LLC, C=US",
  publisherDisplayName: "Orthic Labs", displayName: "SwiftRight", executable: "App.exe",
  dependencies: [{ name: "Microsoft.VCLibs.140.00.UWPDesktop", minVersion: "14.0.30704.0" }] };

test("AppxManifest carries identity, full-trust entry point and package dependency; escapes XML", () => {
  const x = generateAppxManifest({ ...identity, displayName: 'A & "B"' });
  assert.match(x, /<Identity Name="Orthic.SwiftRight" Publisher="CN=Damned Ventures LLC, O=Damned Ventures LLC, C=US" Version="0.1.0.0" ProcessorArchitecture="x64"/);
  assert.match(x, /EntryPoint="Windows.FullTrustApplication"/);
  assert.match(x, /rescap:Capability Name="runFullTrust"/);
  assert.match(x, /PackageDependency Name="Microsoft.VCLibs.140.00.UWPDesktop"/);
  assert.match(x, /A &amp; &quot;B&quot;/);
});

test("MSIX identity validation rejects bad publisher, version and architecture", () => {
  assert.throws(() => generateAppxManifest({ ...identity, publisher: "CN=Someone Else" }), { code: "MSIX_INVALID_IDENTITY" });
  assert.throws(() => generateAppxManifest({ ...identity, version: "1.0" }), { code: "MSIX_INVALID_IDENTITY" });
  assert.throws(() => generateAppxManifest({ ...identity, architecture: "x86" }), { code: "MSIX_INVALID_IDENTITY" });
});

const PNG = Buffer.concat([Buffer.from("89504e470d0a1a0a", "hex"), Buffer.alloc(8)]);
test("MSIX root assembly requires PNG assets, executable and a flat stage", () => {
  const root = tmp();
  write(root, "stage/App.exe", "x"); write(root, "stage/a.dll", "y");
  const assets = { storeLogo: write(root, "a/s.png", PNG), square150: write(root, "a/l.png", PNG), square44: write(root, "a/m.png", PNG) };
  const pkg = assembleMsixRoot({ stageDir: path.join(root, "stage"), root: path.join(root, "pkg"), manifest: identity, assets });
  assert.ok(existsSync(path.join(pkg, "AppxManifest.xml")));
  assert.ok(existsSync(path.join(pkg, "Assets", "Square44x44Logo.png")));
  assert.deepEqual(peFilesIn(pkg).map((f) => path.basename(f)), ["App.exe", "a.dll"]);
  assert.throws(() => assembleMsixRoot({ stageDir: path.join(root, "stage"), root: path.join(root, "p2"), manifest: identity, assets: { ...assets, storeLogo: write(root, "a/bad.png", "no") } }), { code: "MSIX_MISSING_ASSET" });
  write(root, "stage/sub/c.dll", "z");
  assert.throws(() => assembleMsixRoot({ stageDir: path.join(root, "stage"), root: path.join(root, "p3"), manifest: identity, assets }), { code: "MSIX_NESTED_STAGE" });
  rmSync(root, { recursive: true });
});

test("buildMsix signs PE files, packs, signs the package, then verifies — in that order, and refuses off Windows", () => {
  const root = tmp();
  write(root, "stage/App.exe", "x");
  const assets = { storeLogo: write(root, "s.png", PNG), square150: write(root, "l.png", PNG), square44: write(root, "m.png", PNG) };
  const out = path.join(root, "out", "App.msix");
  const calls = [];
  const run = (cmd, args) => {
    calls.push([path.win32.basename(cmd), ...args.map((a) => (a.length > 3 && a.includes(path.sep) ? path.basename(a) : a))]);
    if (path.win32.basename(cmd) === "makeappx.exe") writeFileSync(out, "msix");
    return { status: 0, stdout: "", stderr: "" };
  };
  const logs = [];
  assert.throws(() => buildMsix({ stageDir: path.join(root, "stage"), out, manifest: identity, assets }, { platform: "darwin", log: () => {} }), { code: "MSIX_NATIVE_HOST_REQUIRED" });
  const result = buildMsix({ stageDir: path.join(root, "stage"), out, manifest: identity, assets },
    { platform: "win32", run, findTool: () => ({ toolPath: "C:\\sdk\\makeappx.exe" }), env: {}, log: (e) => logs.push(e), readSubject: () => identity.publisher });
  assert.equal(result.signed, true);
  const kinds = calls.map((c) => (c[0] === "makeappx.exe" ? "pack" : c.includes("--verify-only") ? "verify" : c[1] === "sign-windows.mjs" && c.includes("App.msix") ? "sign-msix" : "sign-pe"));
  assert.deepEqual(kinds, ["sign-pe", "pack", "sign-msix", "verify"]);
  assert.ok(calls[1].includes("pack") && calls[1].includes("/o"));
  const dry = buildMsix({ stageDir: "x", out, manifest: identity, assets, dryRun: true }, { log: () => {} });
  assert.deepEqual(dry.steps, ["assemble", "verify-inventory", "sign-pe", "pack", "sign-msix", "verify-msix", "verify-publisher", "seal"]);
  rmSync(root, { recursive: true });
});

test("Info.plist is well formed and validated", () => {
  const plist = generateInfoPlist({ name: "SwiftRight", bundleId: "dev.orthic.swiftright", version: "0.1.0", executable: "SwiftRight", infoPlistExtra: { LSUIElement: true } });
  assert.match(plist, /<key>CFBundleIdentifier<\/key>\n\t<string>dev.orthic.swiftright<\/string>/);
  assert.match(plist, /<key>LSMinimumSystemVersion<\/key>\n\t<string>14.0<\/string>/);
  assert.match(plist, /<key>LSUIElement<\/key>\n\t<true\/>/);
  assert.throws(() => generateInfoPlist({ name: "X", bundleId: "bad", version: "1", executable: "X" }), { code: "MAC_APP_INVALID" });
});

test("app assembly lays out bundle and finds nested code deepest-first", () => {
  const root = tmp();
  const macho = Buffer.from("cffaedfe00000000", "hex");
  const exe = write(root, "build/SwiftRight", macho); chmodSync(exe, 0o755);
  const dylib = write(root, "fw/libswiftright_services.dylib", macho);
  write(root, "fw/Inner.framework/Versions/A/Inner", macho);
  const bundle = write(root, "res/SwiftRight_SwiftRight.bundle/Info.plist", "<plist/>");
  const result = assembleMacApp({ executable: exe, name: "SwiftRight", bundleId: "dev.orthic.swiftright", version: "0.1.0", out: path.join(root, "out"),
    frameworks: [dylib, path.join(root, "fw/Inner.framework")], resources: [path.dirname(bundle)] }, { platform: "linux" });
  const rel = (p) => path.relative(result.app, p);
  assert.ok(existsSync(path.join(result.app, "Contents/Info.plist")));
  assert.ok(existsSync(path.join(result.app, "Contents/MacOS/SwiftRight")));
  assert.ok(existsSync(path.join(result.app, "Contents/Resources/SwiftRight_SwiftRight.bundle/Info.plist")));
  const nested = result.nested.map(rel);
  assert.ok(nested.includes("Contents/Frameworks/libswiftright_services.dylib"));
  assert.ok(nested.includes("Contents/Frameworks/Inner.framework"));
  assert.ok(nested.indexOf("Contents/Frameworks/Inner.framework/Versions/A/Inner") < nested.indexOf("Contents/Frameworks/Inner.framework"));
  assert.ok(!nested.some((n) => n.startsWith("Contents/MacOS")));
  assert.ok(!nested.some((n) => n.endsWith(".bundle")));
  assert.deepEqual(discoverNestedCode(result.app, "SwiftRight"), result.nested);
  rmSync(root, { recursive: true });
});

test("mac signing is inside-out via sign-macos (entitlements only on bundle) and notarization staples the app", () => {
  const calls = [];
  const run = (cmd, args) => { calls.push([path.win32.basename(cmd), ...args]); return { status: 0 }; };
  const app = "/tmp/Out/SwiftRight.app";
  const r = signAndNotarizeMacApp({ app, nested: ["/tmp/Out/SwiftRight.app/Contents/Frameworks/libx.dylib"], entitlements: "/e.plist", receiptDir: "/tmp/rcpt", notarize: true, profile: "rk" },
    { platform: "darwin", run, log: () => {} });
  const scripts = calls.map((c) => (c[0] === "ditto" ? "ditto" : path.basename(c[1])));
  assert.deepEqual(scripts, ["sign-macos.mjs", "sign-macos.mjs", "ditto", "notarize-macos.mjs"]);
  assert.ok(!calls[0].includes("--entitlements"));
  assert.ok(calls[1].includes("--entitlements"));
  assert.ok(calls[3].includes("--app") && calls[3].includes("rk"));
  assert.equal(r.zip, `${app}.zip`);
  assert.throws(() => signAndNotarizeMacApp({ app, nested: [], receiptDir: "/tmp/r" }, { platform: "linux", run }), { code: "MAC_APP_NATIVE_HOST_REQUIRED" });
  const dry = signAndNotarizeMacApp({ app, nested: [], receiptDir: "/tmp/r", notarize: true, dryRun: true }, { platform: "linux", run: (cmd) => { if (/ditto/.test(cmd)) throw new Error("must not run ditto"); return { status: 0 }; }, log: () => {} });
  assert.equal(dry.plan.length, 3);
});

test("config mapping resolves paths against the config and drives stage + msix + mac options", () => {
  const root = tmp();
  const cfg = write(root, "swift-package.json", JSON.stringify({ schema: 1, name: "swiftright", displayName: "SwiftRight", version: "0.1.0",
    windows: { exe: ".build/App.exe", out: "dist/App.msix", searchDirs: ["swift"], publisher: identity.publisher, publisherDisplayName: "Orthic Labs", assets: { storeLogo: "a/s.png" } },
    mac: { executable: ".build/SwiftRight", bundleId: "dev.orthic.swiftright", out: "dist/mac", entitlements: "e.plist" } }));
  const loaded = loadSwiftPackageConfig(cfg);
  const stage = windowsStageOptions(loaded);
  assert.equal(stage.exe, path.join(root, ".build/App.exe"));
  assert.equal(stage.out, path.join(root, "dist/App.msix.stage"));
  const m = msixOptions(loaded, { architecture: "arm64", exe: "App.exe", out: stage.out });
  assert.equal(m.manifest.version, "0.1.0.0");
  assert.equal(m.manifest.architecture, "arm64");
  assert.equal(m.assets.storeLogo, path.join(root, "a/s.png"));
  assert.equal(macOptions(loaded).name, "SwiftRight");
  assert.throws(() => loadSwiftPackageConfig(write(root, "bad.json", JSON.stringify({ schema: 2 }))), { code: "SWIFT_PACKAGE_CONFIG" });
  rmSync(root, { recursive: true });
});

test("command entry stages end-to-end and rejects unknown commands", async () => {
  const root = tmp();
  write(root, "App.exe", pe({ imports: ["KERNEL32.dll"] }));
  const cfg = write(root, "c.json", JSON.stringify({ schema: 1, name: "x", version: "1.0.0", windows: { exe: "App.exe", out: "o.msix" } }));
  let result;
  await main(["windows-stage", "--config", cfg], { log: (v) => { result = v; } });
  assert.equal(result.files[0].name, "App.exe");
  await assert.rejects(main(["bogus"]), { code: "SWIFT_PACKAGE_USAGE" });
  rmSync(root, { recursive: true });
});
