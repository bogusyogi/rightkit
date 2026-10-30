// rightkit#1303: `development.targets.win.install.kind: "tauri-authenticode"`. Fakes only: no signtool, no NSIS, no installer is run. The order is the release's
// (build --no-bundle, patch the marker, sign, bundle, check the payload, sign the installer, install, verify) and nothing installs before the installer verified.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { validateDevelopmentConfig } from "./development.mjs";
import { TAURI_AUTHENTICODE_KIND, TAURI_AUTHENTICODE_ORDER, authenticodeBundleCommand, installTauriAuthenticode, resolveAuthenticodeDescriptor, validateAuthenticodeBuild } from "./install-tauri-authenticode.mjs";
import { SIGN_SUBJECT } from "./suite-signing-policy.mjs";

const sha = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const DESCRIPTOR = { kind: "tauri-authenticode", productName: "RightApp", targetTriple: "x86_64-pc-windows-msvc", manifestPath: "src-tauri/Cargo.toml", expectedInstalledPath: "C:/Users/a/AppData/Local/RightApp/rightapp.exe", sidecars: ["engine.exe"] };
const BUILD = [{ cmd: "right-release", args: ["stage-sidecar", "--target", "x86_64-pc-windows-msvc"] }, { cmd: "pnpm", args: ["exec", "tauri", "build", "--target", "x86_64-pc-windows-msvc", "--no-bundle", "--", "--profile", "dev", "--locked"] }];

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-authenticode-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const debugDir = path.join(dir, "debug");
  fs.mkdirSync(debugDir);
  fs.writeFileSync(path.join(debugDir, "rightapp.exe"), "MZ-unsigned-exe");
  fs.writeFileSync(path.join(debugDir, "engine.exe"), "MZ-unsigned-engine");
  const installer = path.join(dir, "RightApp_0.1.0_x64-setup.exe");
  return { dir, debugDir, installer };
}

/** Fake deps that log every effect in order. `fail` names a step to break; `subject` is what the verifier reports. */
function deps(f, { fail, subject = SIGN_SUBJECT, embeddedSha } = {}) {
  const log = []; const seen = {};
  const record = (file) => ({ file, before: { sha256: "0".repeat(64) }, after: { sha256: sha(file) }, authenticode: "Valid", subject, timestampPresent: true });
  const base = (file) => path.basename(file);
  return {
    log, seen,
    deps: {
      patchMarker: (file) => { log.push(`patch:${base(file)}`); if (fail === "patch") throw new Error("two NSIS markers"); fs.appendFileSync(file, "|NSS"); },
      signWindows: (files) => {
        log.push(`sign:${files.map(base).join(",")}`);
        if (fail === "sign" && base(files[0]) === "rightapp.exe") throw new Error("Azure Artifact Signing refused");
        if (fail === "sign-installer" && base(files[0]).endsWith("setup.exe")) throw new Error("installer signing refused");
        for (const file of files) fs.appendFileSync(file, "|AUTHENTICODE");
        return files.map(record);
      },
      verifyWindows: (files) => { log.push(`verify:${files.map(base).join(",")}`); return files.map((file) => ({ ...record(file), subject: fail === "verify-installer" && base(file).endsWith("setup.exe") ? "CN=Someone Else" : fail === "verify-exe" && base(file) === "rightapp.exe" ? "CN=Someone Else" : subject })); },
      bundle: (command) => { log.push("bundle"); if (fail === "bundle") throw new Error("tauri bundle failed"); fs.writeFileSync(f.installer, `NSIS(${fs.readFileSync(path.join(f.debugDir, "rightapp.exe"), "utf8")})`); seen.command = command; },
      findInstaller: () => { log.push("find-installer"); return f.installer; },
      verifyPayload: ({ installer, entryName, expectedSha256 }) => {
        log.push("payload");
        if (fail === "payload" || (embeddedSha && embeddedSha !== expectedSha256)) throw new Error(`the installer does not embed the signed ${entryName}`);
        return { installer, entry: entryName, sha256: expectedSha256 };
      },
      install: (installer) => { log.push(`install:${base(installer)}`); },
      installedExe: () => { log.push("installed-exe"); return fail === "verify-installed-missing" ? path.join(f.dir, "gone.exe") : path.join(f.debugDir, "rightapp.exe"); },
    },
  };
}

test("the order is the release's, with nothing installed before the installer is signed and verified", (t) => {
  const f = fixture(t);
  const d = deps(f);
  const result = installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, deps: d.deps });
  assert.deepEqual(result.order, [...TAURI_AUTHENTICODE_ORDER]);
  assert.deepEqual(d.log, [
    "patch:rightapp.exe",
    "sign:rightapp.exe,engine.exe", "verify:rightapp.exe,engine.exe",
    "bundle", "find-installer", "payload",
    "sign:RightApp_0.1.0_x64-setup.exe", "verify:RightApp_0.1.0_x64-setup.exe",
    "install:RightApp_0.1.0_x64-setup.exe", "installed-exe", "verify:rightapp.exe",
  ]);
  // The marker is patched BEFORE the first byte is signed (Tauri rewrites it during bundling; doing that to signed bytes breaks Authenticode).
  assert.ok(d.log.indexOf("patch:rightapp.exe") < d.log.findIndex((line) => line.startsWith("sign:")));
  // The payload check is against the exe as it was signed, and the bundle command is the carried Tauri bundle with no Tauri-side signing and no identifier.
  assert.deepEqual(d.seen.command.args.slice(0, 7), ["exec", "tauri", "bundle", "--bundles", "nsis", "--debug", "--target"]);
  assert.equal(d.seen.command.cmd, "pnpm");
  const override = JSON.parse(d.seen.command.args[d.seen.command.args.indexOf("--config") + 1]);
  assert.deepEqual(override, { bundle: { createUpdaterArtifacts: false, windows: { signCommand: null } } });
  assert.equal(result.payload.sha256, sha(path.join(f.debugDir, "rightapp.exe")));
  assert.equal(result.signing.subject, "CN=Damned Ventures LLC");
  assert.deepEqual(result.signing.files.map((file) => path.basename(file.file)), ["rightapp.exe", "engine.exe", "RightApp_0.1.0_x64-setup.exe"]);
});

test("an unsigned installer is never run: every failure before the installer verified stops before `install`", (t) => {
  for (const fail of ["patch", "sign", "verify-exe", "bundle", "payload", "sign-installer", "verify-installer"]) {
    const f = fixture(t);
    const d = deps(f, { fail });
    assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, deps: d.deps }), Error, fail);
    assert.equal(d.log.some((line) => line.startsWith("install:")), false, `${fail}: nothing was installed`);
  }
  // The payload check itself: an installer that embeds other bytes than the signed ones is refused.
  const f = fixture(t);
  const d = deps(f, { embeddedSha: "f".repeat(64) });
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, deps: d.deps }), /does not embed the signed/);
  assert.equal(d.log.some((line) => line.startsWith("install:")), false);
});

test("a signer that reports another subject, or an invalid signature, is refused even when it did not throw", (t) => {
  const f = fixture(t);
  const d = deps(f, { subject: "CN=Someone Else" });
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, deps: d.deps }), (error) => error.code === "TAURI_AUTHENTICODE_SIGN" && /CN=Damned Ventures LLC/.test(error.message));
  assert.equal(d.log.some((line) => line.startsWith("bundle") || line.startsWith("install:")), false);
  const g = fixture(t);
  const e = deps(g);
  e.deps.signWindows = (files) => files.map((file) => ({ file, subject: SIGN_SUBJECT, authenticode: "Invalid", before: {}, after: {} }));
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: g.debugDir, deps: e.deps }), /Valid/);
});

test("the install is verified last: a signature that does not verify on the installed exe is reported, after the (already signed) install", (t) => {
  const f = fixture(t);
  const d = deps(f);
  const verify = d.deps.verifyWindows;
  let calls = 0;
  d.deps.verifyWindows = (files) => { calls += 1; return verify(files).map((item) => (calls === 3 ? { ...item, subject: "CN=Someone Else" } : item)); };
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, deps: d.deps }), (error) => error.code === "TAURI_AUTHENTICODE_SIGN");
  assert.ok(d.log.some((line) => line.startsWith("install:")), "the verification of the install comes after the install, and the installer it ran was verified");
});

test("a missing build output or sidecar is refused before anything is touched", (t) => {
  const f = fixture(t);
  fs.rmSync(path.join(f.debugDir, "rightapp.exe"));
  const d = deps(f);
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, deps: d.deps }), (error) => error.code === "TAURI_AUTHENTICODE_BUILD_MISSING");
  assert.deepEqual(d.log, []);
  const g = fixture(t);
  fs.rmSync(path.join(g.debugDir, "engine.exe"));
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: g.debugDir, deps: deps(g).deps }), (error) => error.code === "TAURI_AUTHENTICODE_BUILD_MISSING");
});

test("the descriptor declares no identity: unknown keys, a thumbprint and escaping paths are refused", () => {
  assert.equal(resolveAuthenticodeDescriptor(DESCRIPTOR).productName, "RightApp");
  const code = (over) => { try { resolveAuthenticodeDescriptor({ ...DESCRIPTOR, ...over }); } catch (error) { return error.code; } return null; };
  assert.equal(code({ thumbprint: "AB" }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ signer: "CN=x" }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ targetTriple: "aarch64-apple-darwin" }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ expectedInstalledPath: undefined }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ manifestPath: undefined }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ manifestPath: "../x/Cargo.toml" }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ sidecars: ["../engine.exe"] }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ sidecars: ["C:\\engine.exe"] }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ sidecars: "engine.exe" }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(code({ kind: "tauri" }), "TAURI_AUTHENTICODE_CONFIG");
  assert.equal(TAURI_AUTHENTICODE_KIND, "tauri-authenticode");
  assert.equal(authenticodeBundleCommand({ ...DESCRIPTOR, tauriCwd: "src-tauri" }).cwd, "src-tauri");
});

test("the build must not bundle: `tauri build --no-bundle` is required, a bundle or --bundles is refused, and an identifier override is refused", () => {
  assert.doesNotThrow(() => validateAuthenticodeBuild(BUILD));
  const code = (build, readFile) => { try { validateAuthenticodeBuild(build, undefined, readFile); } catch (error) { return error.code; } return null; };
  assert.equal(code([{ cmd: "pnpm", args: ["exec", "tauri", "build", "--bundles", "nsis"] }]), "TAURI_AUTHENTICODE_BUILD");
  assert.equal(code([{ cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle", "--bundles", "nsis"] }]), "TAURI_AUTHENTICODE_BUILD");
  assert.equal(code([{ cmd: "pnpm", args: ["exec", "tauri", "build"] }]), "TAURI_AUTHENTICODE_BUILD");
  assert.equal(code([{ cmd: "pnpm", args: ["exec", "tauri", "bundle", "--bundles", "nsis"] }]), "TAURI_AUTHENTICODE_BUILD");
  assert.equal(code([{ cmd: "right-release", args: ["stage-sidecar"] }]), "TAURI_AUTHENTICODE_BUILD", "no tauri build at all");
  assert.equal(code([]), "TAURI_AUTHENTICODE_BUILD");
  assert.equal(code([{ cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle", "--config", '{"identifier":"a.b"}'] }]), "TAURI_AUTHENTICODE_BUILD");
  assert.equal(code([{ cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle", "--config", "x.json"] }], () => null), "TAURI_AUTHENTICODE_BUILD", "a --config file that cannot be read");
  assert.equal(code([{ cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle", "--config", "x.json"] }], () => '{"bundle":{}}'), null);
});

test("development config: the signed Windows kind validates for win only, with its build checked; the unsigned `tauri` kind still validates (the gate refuses it on the Dell)", () => {
  const config = (install, build = BUILD) => ({ development: { targets: { win: { build, install }, mac: { build: [{ cmd: "node", args: ["x.mjs"] }], install } } } });
  assert.doesNotThrow(() => validateDevelopmentConfig(config(DESCRIPTOR), "win", { root: process.cwd() }));
  assert.throws(() => validateDevelopmentConfig(config(DESCRIPTOR), "mac", { root: process.cwd() }), /--platform win/);
  assert.throws(() => validateDevelopmentConfig(config(DESCRIPTOR, [{ cmd: "pnpm", args: ["exec", "tauri", "build", "--bundles", "nsis"] }]), "win", { root: process.cwd() }), /no-bundle/);
  assert.throws(() => validateDevelopmentConfig(config({ ...DESCRIPTOR, thumbprint: "AB" }), "win", { root: process.cwd() }), /unknown key/);
  assert.doesNotThrow(() => validateDevelopmentConfig(config({ kind: "tauri", productName: "RightApp", targetTriple: "x86_64-pc-windows-msvc", expectedInstalledPath: "x" }), "win", { root: process.cwd() }));
});
