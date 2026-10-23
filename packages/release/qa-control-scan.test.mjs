import assert from "node:assert/strict";
import { mkdirSync, readFileSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { deflateRawSync } from "node:zlib";
import {
  DEV_INSPECT_MARKER, QA_CONTROL_MARKER, assertNoQaControl, bufferHasQaControlMarker, bufferQaFindings, fileHasQaControlMarker, isNativeBinaryHeader, scanForQaControl, scanWithStats, WEB_MOCK_MARKERS, assertNoWebBundleMocks, scanWebBundle, webBundleMockFindings,
} from "./qa-control-scan.mjs";
import { registryEvidenceFixture } from "./test-support/admission.mjs";

const MACHO = Buffer.from([0xcf, 0xfa, 0xed, 0xfe]);
const PE = Buffer.from("MZ");
const filler = (n) => Buffer.alloc(n, 0x90);
const withMarker = (header, before = 100, after = 100) =>
  Buffer.concat([header, filler(before), Buffer.from(QA_CONTROL_MARKER), filler(after)]);
const without = (header) => Buffer.concat([header, filler(500), Buffer.from("RIGHTKIT-QA-CONTROL")]);

test("marker is the fixed string the Rust crate emits", () => {
  assert.equal(QA_CONTROL_MARKER, "RIGHTKIT-QA-CONTROL-SERVER-PRESENT");
});

test("native headers: Mach-O thin and fat, PE, ELF; not text", () => {
  for (const head of [[0xfe, 0xed, 0xfa, 0xcf], [0xcf, 0xfa, 0xed, 0xfe], [0xca, 0xfe, 0xba, 0xbe], [0x7f, 0x45, 0x4c, 0x46], [0x4d, 0x5a, 0, 0]]) {
    assert.equal(isNativeBinaryHeader(Buffer.from(head)), true);
  }
  assert.equal(isNativeBinaryHeader(Buffer.from("text")), false);
});

test("a Mach-O and a PE fixture containing the marker are findings; clean ones are not", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const bad = [join(root, "app-macho"), join(root, "app.exe")];
  const good = [join(root, "clean-macho"), join(root, "clean.exe")];
  writeFileSync(bad[0], withMarker(MACHO));
  writeFileSync(bad[1], withMarker(PE));
  writeFileSync(good[0], without(MACHO));
  writeFileSync(good[1], without(PE));
  assert.deepEqual(scanForQaControl(good), []);
  const findings = scanForQaControl(bad);
  assert.equal(findings.length, 2);
  assert.throws(() => assertNoQaControl(bad, "release"), /QA control server and every other test-automation or debugging surface must never ship/);
  assert.doesNotThrow(() => assertNoQaControl(good, "release"));
});

test("a .app directory is walked recursively; a text file with the marker is not a binary", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const exe = join(root, "X.app", "Contents", "MacOS");
  mkdirSync(exe, { recursive: true });
  writeFileSync(join(exe, "x"), withMarker(MACHO));
  writeFileSync(join(root, "X.app", "Contents", "notes.txt"), QA_CONTROL_MARKER);
  const findings = scanForQaControl([join(root, "X.app")]);
  assert.deepEqual(findings.map((f) => f.file), [join(exe, "x")]);
});

test("a marker split across the chunk boundary is found, and symlinks are not followed", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const big = join(root, "big");
  const chunk = 4 * 1024 * 1024;
  writeFileSync(big, Buffer.concat([MACHO, filler(chunk - 4 - 10), Buffer.from(QA_CONTROL_MARKER), filler(20)]));
  assert.equal(fileHasQaControlMarker(big), true);
  const link = join(root, "link");
  symlinkSync(big, link);
  const dir = join(root, "d");
  mkdirSync(dir);
  symlinkSync(big, join(dir, "l"));
  assert.deepEqual(scanForQaControl([dir]), []);
});

test("an unreadable or missing path fails closed", () => {
  const findings = scanForQaControl([join(tmpdir(), "rk-qa-scan-does-not-exist")]);
  assert.equal(findings.length, 1);
  assert.match(findings[0].reason, /could not scan/);
});

test("buffer helper", () => {
  assert.equal(bufferHasQaControlMarker(withMarker(PE)), true);
  assert.equal(bufferHasQaControlMarker(without(PE)), false);
});

test("hardeningscan fails an artifact that carries the marker and passes one that does not", async () => {
  const { spawnSync } = await import("./background-process.mjs");
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const bad = join(root, "bad.exe");
  const good = join(root, "good.exe");
  writeFileSync(bad, withMarker(PE));
  writeFileSync(good, without(PE));
  const scanner = join(import.meta.dirname, "hardeningscan.mjs");
  const failed = spawnSync(process.execPath, [scanner, bad], { cwd: root, encoding: "utf8" });
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /QA control server shipped/);
  const passed = spawnSync(process.execPath, [scanner, good], { cwd: root, encoding: "utf8" });
  assert.equal(passed.status, 0, passed.stderr);
});

test("a sealed release containing the marker fails verification (every seal and upload path)", async () => {
  const { verifySealedRelease } = await import("./release-state.mjs");
  const { createHash } = await import("node:crypto");
  const make = (bytes) => {
    const dir = mkdtempSync(join(tmpdir(), "rk-qa-sealed-"));
    writeFileSync(join(dir, "App.exe"), bytes);
    writeFileSync(join(dir, "release-manifest.json"), JSON.stringify({
      schema: 1, ...registryEvidenceFixture(), checkpoints: ["sealed"],
      files: [{ name: "App.exe", sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: bytes.length }],
    }));
    return dir;
  };
  assert.doesNotThrow(() => verifySealedRelease(make(without(PE))));
  assert.throws(() => verifySealedRelease(make(withMarker(PE))), /QA control server/);
});

// ---- second rule: tauri-plugin-wdio-webdriver (two of three independent signatures) -------------------------
const WDIO_CRATE = "tauri-plugin-wdio-webdriver-1.4.0/src/server.rs";
const WDIO_MODULE = "tauri_plugin_wdio_webdriver::init";
const WDIO_PERMISSION = "wdio-webdriver:default";
const bytesWith = (header, ...strings) => Buffer.concat([header, filler(64), ...strings.flatMap((t) => [Buffer.from(t), filler(32)])]);
const rules = (buffer) => bufferQaFindings(buffer).map((f) => f.rule);

test("wdio rule: two signatures fire, one alone does not, any pair does, UTF-16 counts", () => {
  assert.deepEqual(rules(bytesWith(PE, WDIO_CRATE, WDIO_MODULE)), ["wdio-webdriver"]);
  assert.deepEqual(rules(bytesWith(MACHO, WDIO_MODULE, WDIO_PERMISSION)), ["wdio-webdriver"]);
  assert.deepEqual(rules(bytesWith(PE, WDIO_CRATE, WDIO_PERMISSION)), ["wdio-webdriver"]);
  assert.deepEqual(rules(bytesWith(PE, WDIO_CRATE)), []);
  assert.deepEqual(rules(bytesWith(PE, WDIO_MODULE)), []);
  assert.deepEqual(rules(bytesWith(PE, WDIO_PERMISSION)), []);
  assert.deepEqual(rules(Buffer.concat([PE, filler(10), Buffer.from(WDIO_CRATE, "utf16le"), filler(10), Buffer.from(WDIO_MODULE)])), ["wdio-webdriver"]);
  // the same signature twice is still one signature
  assert.deepEqual(rules(bytesWith(PE, WDIO_CRATE, WDIO_CRATE)), []);
});

test("wdio signatures split across a chunk boundary are found", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const chunk = 4 * 1024 * 1024;
  const file = join(root, "app.exe");
  writeFileSync(file, Buffer.concat([PE, filler(chunk - 2 - 5), Buffer.from(WDIO_MODULE), filler(50), Buffer.from(WDIO_PERMISSION)]));
  const findings = scanForQaControl([file]);
  assert.deepEqual(findings.map((f) => f.rule), ["wdio-webdriver"]);
  assert.match(findings[0].reason, /2 of 3 signatures/);
});

test("remote-debugging and tauri-devtools strings are findings; a clean binary with similar text is not", () => {
  assert.deepEqual(rules(bytesWith(PE, "--remote-debugging-port=9222")), ["remote-debugging"]);
  assert.deepEqual(rules(bytesWith(PE, "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS")), ["remote-debugging"]);
  assert.deepEqual(rules(Buffer.concat([PE, Buffer.from("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "utf16le")])), ["remote-debugging"]);
  assert.deepEqual(rules(bytesWith(MACHO, "plugin:webview|internal_toggle_devtools")), ["tauri-devtools"]);
  assert.deepEqual(rules(bytesWith(MACHO, "allow-internal-toggle-devtools", "remote-debugging", "wdio")), []);
  assert.deepEqual(rules(bytesWith(PE, QA_CONTROL_MARKER, "--remote-debugging-port")).sort(), ["qa-control-server", "remote-debugging"]);
});

test("dev-inspect marker: the fixed string rightkit-tauri-shell emits, a finding in any encoding, never lifted by the devtools allow-list", () => {
  assert.equal(DEV_INSPECT_MARKER, "RIGHTKIT-SHELL-DEV-INSPECT-COMPILED");
  const rust = readFileSync(join(import.meta.dirname, "..", "..", "crates", "rightkit-tauri-shell", "src", "devtools.rs"), "utf8");
  assert.ok(rust.includes(`"${DEV_INSPECT_MARKER}"`), "devtools.rs must define the same marker");
  assert.deepEqual(rules(bytesWith(MACHO, DEV_INSPECT_MARKER)), ["shell-dev-inspect"]);
  assert.deepEqual(rules(bytesWith(PE, DEV_INSPECT_MARKER)), ["shell-dev-inspect"]);
  assert.deepEqual(rules(Buffer.concat([PE, filler(10), Buffer.from(DEV_INSPECT_MARKER, "utf16le")])), ["shell-dev-inspect"]);
  // a near miss (the content-only marker, a prefix) is not this rule
  assert.deepEqual(rules(bytesWith(PE, "RIGHTKIT-SHELL-DEV-INSPECT")), []);
  // an allow-listed ContentOnly app that also carries the content marker still fails on dev-inspect
  const both = bytesWith(PE, "RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED", "plugin:webview|internal_toggle_devtools", DEV_INSPECT_MARKER);
  assert.deepEqual(bufferQaFindings(both, { allowContentOnlyDevtools: true }).map((f) => f.rule), ["shell-dev-inspect"]);
  assert.match(bufferQaFindings(both)[0].reason, /contains RIGHTKIT-SHELL-DEV-INSPECT-COMPILED|signatures/);
  // the content-only marker alone is not a dev-inspect finding
  assert.deepEqual(rules(bytesWith(PE, "RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED")), []);
  // split across the chunk boundary
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const file = join(root, "app.exe");
  const chunk = 4 * 1024 * 1024;
  writeFileSync(file, Buffer.concat([PE, filler(chunk - 2 - 10), Buffer.from(DEV_INSPECT_MARKER), filler(20)]));
  assert.deepEqual(scanForQaControl([file]).map((f) => f.rule), ["shell-dev-inspect"]);
  assert.throws(() => assertNoQaControl([file], "release"), /shell-dev-inspect|DEV-INSPECT/);
});

// ---- ZIP containers (MSIX/APPX): stored and deflate entries --------------------------------------------------
function zip(entries) {
  const locals = []; const central = []; let offset = 0;
  for (const { name, data, method = 0, flags = 0 } of entries) {
    const nameBuf = Buffer.from(name);
    const body = method === 8 ? deflateRawSync(data) : data;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(flags, 6); local.writeUInt16LE(method, 8);
    local.writeUInt32LE(body.length, 18); local.writeUInt32LE(data.length, 22); local.writeUInt16LE(nameBuf.length, 26);
    const header = Buffer.alloc(46);
    header.writeUInt32LE(0x02014b50, 0); header.writeUInt16LE(20, 4); header.writeUInt16LE(20, 6); header.writeUInt16LE(flags, 8); header.writeUInt16LE(method, 10);
    header.writeUInt32LE(body.length, 20); header.writeUInt32LE(data.length, 24); header.writeUInt16LE(nameBuf.length, 28); header.writeUInt32LE(offset, 42);
    locals.push(local, nameBuf, body); central.push(header, nameBuf);
    offset += 30 + nameBuf.length + body.length;
  }
  const directory = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}
const msix = (entries) => {
  const dir = mkdtempSync(join(tmpdir(), "rk-qa-msix-"));
  const file = join(dir, "App.msix");
  writeFileSync(file, zip(entries));
  return file;
};
const marked = withMarker(PE, 3000, 3000);
const wdio = bytesWith(PE, WDIO_CRATE, WDIO_MODULE);
const manifestXml = Buffer.from("<Package/>");

test("an MSIX with a marked PE is a finding whether the entry is stored or deflated", () => {
  for (const method of [0, 8]) {
    const file = msix([{ name: "AppxManifest.xml", data: manifestXml, method }, { name: "App/App.exe", data: marked, method }]);
    const findings = scanForQaControl([file]);
    assert.equal(findings.length, 1, `method ${method}`);
    assert.equal(findings[0].file, `${file}!/App/App.exe`);
    assert.equal(findings[0].rule, "qa-control-server");
    assert.throws(() => assertNoQaControl([file], "msix package"), /App\.exe/);
  }
});

test("an MSIX with the wdio plugin is a finding, deflated or stored; a clean MSIX passes and counts its binaries", () => {
  for (const method of [0, 8]) {
    assert.deepEqual(scanForQaControl([msix([{ name: "a.dll", data: wdio, method }])]).map((f) => f.rule), ["wdio-webdriver"]);
  }
  const clean = msix([{ name: "App.exe", data: without(PE), method: 8 }, { name: "x.dll", data: without(PE), method: 0 }, { name: "logo.png", data: filler(100), method: 8 }]);
  const { findings, stats } = scanWithStats([clean]);
  assert.deepEqual(findings, []);
  assert.equal(stats.binaries, 2);
  assert.equal(stats.containers, 1);
  assert.doesNotThrow(() => assertNoQaControl([clean], "msix package", { requireBinary: true }));
});

test("a renamed binary and a nested container are still scanned", () => {
  const inner = zip([{ name: "deep.exe", data: marked, method: 8 }]);
  const file = msix([{ name: "Assets/logo.png", data: marked, method: 0 }, { name: "bundle/inner.msix", data: inner, method: 0 }]);
  assert.deepEqual(scanForQaControl([file]).map((f) => f.file.split("!/").slice(1).join("!/")), ["Assets/logo.png", "bundle/inner.msix!/deep.exe"]);
});

test("containers fail closed: encrypted entry, unknown method, corrupt or truncated data, oversize, and no binary", () => {
  const encrypted = msix([{ name: "a.exe", data: marked, flags: 1 }]);
  assert.match(scanForQaControl([encrypted])[0].reason, /encrypted/);
  const unknown = msix([{ name: "a.exe", data: marked, method: 0 }]);
  const bytes = Buffer.from(zip([{ name: "a.exe", data: marked, method: 0 }]));
  bytes.writeUInt16LE(14, bytes.indexOf(Buffer.from([0x50, 0x4b, 0x01, 0x02])) + 10); // LZMA
  writeFileSync(unknown, bytes);
  assert.match(scanForQaControl([unknown])[0].reason, /unsupported compression method 14/);
  const truncated = join(mkdtempSync(join(tmpdir(), "rk-qa-msix-")), "t.msix");
  writeFileSync(truncated, zip([{ name: "a.exe", data: marked }]).subarray(0, 120));
  assert.match(scanForQaControl([truncated])[0].reason, /invalid ZIP container/);
  const big = msix([{ name: "a.exe", data: marked, method: 8 }]);
  assert.match(scanWithStats([big], { limits: { maxEntryBytes: 100 } }).findings[0].reason, /larger than/);
  assert.match(scanWithStats([big], { limits: { totalInflated: 100 } }).findings[0].reason, /inflate budget/);
  const empty = msix([{ name: "AppxManifest.xml", data: manifestXml }]);
  assert.deepEqual(scanForQaControl([empty]), []);
  assert.throws(() => assertNoQaControl([empty], "msix package", { requireBinary: true }), /no Mach-O, PE or ELF/);
  const missing = join(tmpdir(), "rk-qa-stage-missing");
  assert.throws(() => assertNoQaControl([missing], "msix staged payload", { requireBinary: true }), /could not scan/);
  const stageEmpty = mkdtempSync(join(tmpdir(), "rk-qa-stage-"));
  assert.throws(() => assertNoQaControl([stageEmpty], "msix staged payload", { requireBinary: true }), /no Mach-O, PE or ELF/);
});

test("a staged directory (MSIX payload, NSIS payload, .app in a DMG staging dir) is scanned before packing", () => {
  const stage = mkdtempSync(join(tmpdir(), "rk-qa-stage-"));
  mkdirSync(join(stage, "App.app", "Contents", "MacOS"), { recursive: true });
  writeFileSync(join(stage, "App.exe"), without(PE));
  writeFileSync(join(stage, "plugin.dll"), wdio);
  writeFileSync(join(stage, "App.app", "Contents", "MacOS", "app"), bytesWith(MACHO, "plugin:webview|internal_toggle_devtools"));
  const findings = scanForQaControl([stage]);
  assert.deepEqual(findings.map((f) => f.rule).sort(), ["tauri-devtools", "wdio-webdriver"]);
});

test("what stays opaque after packing is reported: NSIS payload and compressed DMG", () => {
  const dir = mkdtempSync(join(tmpdir(), "rk-qa-opaque-"));
  writeFileSync(join(dir, "setup.exe"), Buffer.concat([PE, filler(64), Buffer.from("NullsoftInst"), filler(64)]));
  writeFileSync(join(dir, "App.dmg"), Buffer.concat([Buffer.from("not-a-readable-image"), filler(1000)]));
  writeFileSync(join(dir, "plain.exe"), without(PE));
  const { findings, stats } = scanWithStats([dir]);
  assert.deepEqual(findings, []);
  assert.deepEqual(stats.opaque.map((o) => [o.file.split(/[\\/]/).pop(), o.kind]).sort(), [["App.dmg", "dmg"], ["setup.exe", "nsis"]]);
});

test("a sealed MSIX with a marked PE inside fails verification", async () => {
  const { verifySealedRelease } = await import("./release-state.mjs");
  const { createHash } = await import("node:crypto");
  const make = (bytes) => {
    const dir = mkdtempSync(join(tmpdir(), "rk-qa-sealed-"));
    writeFileSync(join(dir, "App.msix"), bytes);
    writeFileSync(join(dir, "release-manifest.json"), JSON.stringify({
      schema: 1, ...registryEvidenceFixture(), checkpoints: ["sealed"],
      files: [{ name: "App.msix", sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: bytes.length }],
    }));
    return dir;
  };
  assert.doesNotThrow(() => verifySealedRelease(make(zip([{ name: "App.exe", data: without(PE), method: 8 }]))));
  assert.throws(() => verifySealedRelease(make(zip([{ name: "App.exe", data: marked, method: 8 }]))), /QA control server/);
  assert.throws(() => verifySealedRelease(make(zip([{ name: "a.dll", data: wdio, method: 8 }]))), /wdio-webdriver/);
});

const GOOD_BUNDLE = 'import{a as b}from"./vendor-1a2b.js";const q=new URLSearchParams(location.search);export function init(){return b("engine")}';
const BAD_BUNDLES = {
  "platform-qa-selector": 'globalThis.__RIGHTKIT_QA_PLATFORM__="windows";',
  "platform-qa-environment": 'const key="RIGHTKIT_QA_PLATFORM";',
  "platform-qa-entry": 'bootstrapQaPlatform({query:location.search});',
  "browser-qa-fake": 'const id="browser-qa-fake";',
  "browser-mock-api": 'import("./browser-mock-api-9f.js")',
  "browser-qa-mock-scheme": 'src="browser-qa-mock://img/1.png"',
  qaMock: 'const m=await import("./qaMock-3c.js")',
  tauriQaMock: "function tauriQaMock(){}",
  isBrowserQaMode: "if(isBrowserQaMode())install()",
  MockDataSource: "class MockDataSource{}",
  "qa-build-flag": 'const on=import.meta.env.VITE_MAILRIGHT_QA==="1";',
  "license-testing-sentinel": 'const s="RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP";',
  "license-testing-entry": 'import("@rightkit/license/testing")',
};

test("web bundle scan: known-good bundle passes, every known-bad marker fails", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-web-scan-"));
  const good = join(root, "good"); mkdirSync(join(good, "assets"), { recursive: true });
  writeFileSync(join(good, "index.html"), '<!doctype html><script type="module" src="/assets/index.js"></script>');
  writeFileSync(join(good, "assets", "index.js"), GOOD_BUNDLE);
  assert.deepEqual(scanWebBundle([good], { requireFiles: true }).findings, []);
  assert.doesNotThrow(() => assertNoWebBundleMocks([good], "release", { requireFiles: true }));
  assert.deepEqual(Object.keys(BAD_BUNDLES).sort(), WEB_MOCK_MARKERS.map((m) => m.id).sort());
  for (const [id, body] of Object.entries(BAD_BUNDLES)) {
    const bad = join(root, `bad-${id}`); mkdirSync(join(bad, "assets"), { recursive: true });
    writeFileSync(join(bad, "assets", "index.js"), GOOD_BUNDLE + body);
    const findings = scanWebBundle([bad]).findings;
    assert.deepEqual(findings.map((f) => f.marker), [id], id);
    assert.throws(() => assertNoWebBundleMocks([bad], "release"), /browser-QA mocks or test-mode markers/);
  }
});

test("web bundle scan: html, css and source maps count; other files and symlinks do not; failures are closed", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-web-scan-"));
  writeFileSync(join(root, "a.html"), "<script>window.MockDataSource=1</script>");
  writeFileSync(join(root, "b.js.map"), '{"sources":["../src/lib/ipc/qaMock.ts"]}');
  writeFileSync(join(root, "notes.txt"), "qaMock");
  assert.deepEqual(scanWebBundle([root]).findings.map((f) => f.marker).sort(), ["MockDataSource", "qaMock"]);
  const outside = mkdtempSync(join(tmpdir(), "rk-web-out-"));
  writeFileSync(join(outside, "x.js"), "qaMock");
  const dir = join(root, "d"); mkdirSync(dir); symlinkSync(join(outside, "x.js"), join(dir, "l.js"));
  assert.deepEqual(scanWebBundle([dir]).findings, []);
  assert.equal(scanWebBundle([join(tmpdir(), "rk-web-scan-missing")]).findings[0].rule, "unscannable");
  assert.equal(scanWebBundle([outside.replace(/x$/, "")].slice(0, 0), { requireFiles: true }).findings[0].rule, "unscannable");
  assert.deepEqual(webBundleMockFindings("nothing here"), []);
});


test("web output scan rejects QA platform selectors & QA entry exports without native headers", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-platform-output-"));
  const file = join(root, "app.js");
  for (const marker of ["__RIGHTKIT_QA_PLATFORM__", "RIGHTKIT_QA_PLATFORM", "bootstrapQaPlatform", "resolveQaPlatform", "configureQaPlatform", "@rightkit/platform-ui/qa"]) {
    writeFileSync(file, `globalThis[${JSON.stringify(marker)}] = "windows";`);
    assert.ok(scanWebBundle([file]).findings.length > 0, marker);
  }
  writeFileSync(file, 'const platform = globalThis.__RIGHTKIT_PLATFORM__;');
  assert.deepEqual(scanWebBundle([file]).findings, []);
});
