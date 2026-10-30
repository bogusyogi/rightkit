// rightkit#1303: `development.targets.win.install.kind: "tauri-authenticode"`. Fakes only: no signtool, no NSIS, no installer is run. The order is the release's
// (build --no-bundle, patch the marker, sign, bundle, check the payload, sign the installer, install, verify) and nothing installs before the installer verified.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { validateDevelopmentConfig } from "./development.mjs";
import { TAURI_AUTHENTICODE_KIND, TAURI_AUTHENTICODE_ORDER, authenticodeBundleCommand, classifyStagedFiles, deriveResourceExecutables, installTauriAuthenticode, resolveAuthenticodeDescriptor, validateAuthenticodeBuild } from "./install-tauri-authenticode.mjs";
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
  // The Tauri directory: the externalBin the bundler reads is src-tauri/binaries/<name>-<triple>.exe.
  const tauriDir = path.join(dir, "src-tauri");
  fs.mkdirSync(path.join(tauriDir, "binaries"), { recursive: true });
  fs.writeFileSync(path.join(tauriDir, "tauri.conf.json"), JSON.stringify({ bundle: { externalBin: ["binaries/engine"] } }));
  fs.writeFileSync(path.join(tauriDir, "binaries", "engine-x86_64-pc-windows-msvc.exe"), "MZ-unsigned-engine");
  const installDir = path.join(dir, "installed");
  fs.mkdirSync(installDir);
  const installer = path.join(dir, "RightApp_0.1.0_x64-setup.exe");
  return { dir, debugDir, tauriDir, installDir, installer, bundled: path.join(tauriDir, "binaries", "engine-x86_64-pc-windows-msvc.exe"), payload: {}, extras: [] };
}

/** Fake deps that log every effect in order. `fail` names a step to break; `subject` is what the verifier reports. */
function deps(f, { fail, subject = SIGN_SUBJECT, embeddedSha, engineFrom, installEngine, installResource, engineAuthenticode = "Valid", staged = {} } = {}) {
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
      verifyWindows: (files) => { log.push(`verify:${files.map(base).join(",")}`); return files.map((file) => ({ ...record(file), ...(file.startsWith(f.installDir) && ["engine.exe", "ffmpeg.exe"].includes(base(file)) ? { authenticode: engineAuthenticode } : {}), subject: fail === "verify-installer" && base(file).endsWith("setup.exe") ? "CN=Someone Else" : fail === "verify-exe" && base(file) === "rightapp.exe" ? "CN=Someone Else" : subject })); },
      bundle: (command) => {
        log.push("bundle"); if (fail === "bundle") throw new Error("tauri bundle failed");
        // The bundler embeds the exe from the build output and the sidecar from src-tauri/binaries (what `engineFrom: "debug"` flips).
        const engineSource = engineFrom === "debug" ? path.join(f.debugDir, "engine.exe") : f.bundled;
        f.payload = { "rightapp.exe": fs.readFileSync(path.join(f.debugDir, "rightapp.exe")), "engine.exe": engineFrom === "stale" ? Buffer.from("MZ-rewritten-engine") : engineFrom === "unsigned" ? Buffer.from("MZ-unsigned-engine") : fs.readFileSync(engineSource) };
        for (const file of fs.readdirSync(path.join(f.tauriDir, "binaries"))) { const name = file.replace(/-x86_64-pc-windows-msvc\.exe$/, ".exe"); if (name !== "engine.exe") f.payload[name] = fs.readFileSync(path.join(f.tauriDir, "binaries", file)); }
        f.resourceFiles = deriveResourceExecutables({ tauriDir: f.tauriDir }); for (const r of f.resourceFiles) f.payload[r.file] = fs.readFileSync(r.copies[0].path);
        fs.writeFileSync(f.installer, `NSIS(${fs.readFileSync(path.join(f.debugDir, "rightapp.exe"), "utf8")})`); seen.command = command;
      },
      listPayload: () => [...Object.keys(f.payload).map((name) => ({ name: `$INSTDIR/${name}` })), { name: "$PLUGINSDIR/System.dll" }, { name: "$PLUGINSDIR/helper.exe" }, ...f.extras.map((name) => ({ name }))],
      classifyStaged: (files) => { log.push("classify"); return new Map(files.map((file) => [file, staged[base(file)] ?? "ignored"])); },
      findInstaller: () => { log.push("find-installer"); return f.installer; },
      verifyPayload: ({ installer, entryName, expectedSha256 }) => {
        log.push("payload");
        if (fail === "payload" || (embeddedSha && embeddedSha !== expectedSha256)) throw new Error(`the installer does not embed the signed ${entryName}`);
        const bytes = f.payload[entryName];
        if (!bytes) throw new Error(`could not extract ${entryName}: no such payload entry`);
        const embedded = createHash("sha256").update(bytes).digest("hex");
        if (embedded !== expectedSha256) throw new Error(`the installer does not embed the signed ${entryName}`);
        return { installer, entry: entryName, sha256: expectedSha256 };
      },
      install: (installer) => {
        log.push(`install:${base(installer)}`);
        fs.writeFileSync(path.join(f.installDir, "rightapp.exe"), f.payload["rightapp.exe"]);
        for (const r of f.resourceFiles) { fs.mkdirSync(path.dirname(path.join(f.installDir, r.installRel)), { recursive: true }); fs.writeFileSync(path.join(f.installDir, r.installRel), installResource === "stale" ? "MZ-other-resource" : f.payload[r.file]); }
        for (const [name, bytes] of Object.entries(f.payload)) if (name !== "rightapp.exe" && name !== "engine.exe" && !f.resourceFiles.some((r) => r.file === name)) fs.writeFileSync(path.join(f.installDir, name), bytes);
        if (installEngine !== "missing") fs.writeFileSync(path.join(f.installDir, "engine.exe"), installEngine === "stale" ? "MZ-other-engine" : f.payload["engine.exe"]);
      },
      installedExe: () => { log.push("installed-exe"); return path.join(f.installDir, "rightapp.exe"); },
    },
  };
}

test("the order is the release's, with nothing installed before the installer is signed and verified", (t) => {
  const f = fixture(t);
  const d = deps(f);
  const result = installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, appRoot: f.dir, deps: d.deps });
  assert.deepEqual(result.order, [...TAURI_AUTHENTICODE_ORDER]);
  assert.deepEqual(d.log, [
    "classify", "patch:rightapp.exe",
    "sign:rightapp.exe,engine.exe,engine-x86_64-pc-windows-msvc.exe", "verify:rightapp.exe,engine.exe,engine-x86_64-pc-windows-msvc.exe",
    "bundle", "find-installer", "payload", "payload",
    "sign:RightApp_0.1.0_x64-setup.exe", "verify:RightApp_0.1.0_x64-setup.exe",
    "install:RightApp_0.1.0_x64-setup.exe", "installed-exe", "verify:rightapp.exe,engine.exe",
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
  assert.deepEqual(result.signing.files.map((file) => path.basename(file.file)), ["rightapp.exe", "engine.exe", "engine-x86_64-pc-windows-msvc.exe", "RightApp_0.1.0_x64-setup.exe"]);
});

test("an unsigned installer is never run: every failure before the installer verified stops before `install`", (t) => {
  for (const fail of ["patch", "sign", "verify-exe", "bundle", "payload", "sign-installer", "verify-installer"]) {
    const f = fixture(t);
    const d = deps(f, { fail });
    assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, appRoot: f.dir, deps: d.deps }), Error, fail);
    assert.equal(d.log.some((line) => line.startsWith("install:")), false, `${fail}: nothing was installed`);
  }
  // The payload check itself: an installer that embeds other bytes than the signed ones is refused.
  const f = fixture(t);
  const d = deps(f, { embeddedSha: "f".repeat(64) });
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, appRoot: f.dir, deps: d.deps }), /does not embed the signed/);
  assert.equal(d.log.some((line) => line.startsWith("install:")), false);
});

test("a signer that reports another subject, or an invalid signature, is refused even when it did not throw", (t) => {
  const f = fixture(t);
  const d = deps(f, { subject: "CN=Someone Else" });
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, appRoot: f.dir, deps: d.deps }), (error) => error.code === "TAURI_AUTHENTICODE_SIGN" && /CN=Damned Ventures LLC/.test(error.message));
  assert.equal(d.log.some((line) => line.startsWith("bundle") || line.startsWith("install:")), false);
  const g = fixture(t);
  const e = deps(g);
  e.deps.signWindows = (files) => files.map((file) => ({ file, subject: SIGN_SUBJECT, authenticode: "Invalid", before: {}, after: {} }));
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: g.debugDir, appRoot: g.dir, deps: e.deps }), /Valid/);
});

test("the install is verified last: a signature that does not verify on the installed exe is reported, after the (already signed) install", (t) => {
  const f = fixture(t);
  const d = deps(f);
  const verify = d.deps.verifyWindows;
  let calls = 0;
  d.deps.verifyWindows = (files) => { calls += 1; return verify(files).map((item) => (calls === 3 ? { ...item, subject: "CN=Someone Else" } : item)); };
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, appRoot: f.dir, deps: d.deps }), (error) => error.code === "TAURI_AUTHENTICODE_SIGN");
  assert.ok(d.log.some((line) => line.startsWith("install:")), "the verification of the install comes after the install, and the installer it ran was verified");
});

test("a missing build output or sidecar is refused before anything is touched", (t) => {
  const f = fixture(t);
  fs.rmSync(path.join(f.debugDir, "rightapp.exe"));
  const d = deps(f);
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, appRoot: f.dir, deps: d.deps }), (error) => error.code === "TAURI_AUTHENTICODE_BUILD_MISSING");
  assert.deepEqual(d.log, []);
  const g = fixture(t);
  fs.rmSync(path.join(g.debugDir, "engine.exe")); fs.rmSync(g.bundled);
  assert.throws(() => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: g.debugDir, appRoot: g.dir, deps: deps(g).deps }), (error) => error.code === "TAURI_AUTHENTICODE_SIDECAR_MISSING");
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

// rightkit#1303 M3: every sidecar the app ships is signed, found in the payload, and verified after install.
const run = (f, d, extra = {}) => installTauriAuthenticode({ descriptor: DESCRIPTOR, debugDir: f.debugDir, appRoot: f.dir, deps: d.deps, ...extra });
const codeOf = (fn) => { try { fn(); } catch (error) { return error.code; } return null; };

test("success: the signed engine's bytes reach the installed path, with its digest recorded at every stage", (t) => {
  const f = fixture(t);
  const d = deps(f);
  const result = run(f, d);
  const engine = result.sidecars.find((item) => item.name === "engine");
  assert.deepEqual(engine.copies.map((copy) => copy.role), ["build-output", "bundler-source"]);
  assert.ok(engine.copies.every((copy) => copy.sha256After === sha(copy.path) && copy.sha256After.length === 64));
  assert.equal(engine.installed.sha256, sha(path.join(f.installDir, "engine.exe")));
  assert.equal(engine.installed.sha256, engine.copies[0].sha256After);
  assert.ok(fs.readFileSync(path.join(f.installDir, "engine.exe"), "utf8").endsWith("|AUTHENTICODE"), "the installed engine is the signed one");
  assert.equal(result.payload.sidecars[0].sha256, engine.copies[0].sha256After);
  assert.ok(result.signing.files.some((file) => path.basename(file.file) === "engine-x86_64-pc-windows-msvc.exe" && file.sha256After));
});

test("the sidecar set is derived: externalBin (windows override replaces the base list) and config.sidecars are required with no install.sidecars declared", (t) => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.tauriDir, "tauri.windows.conf.json"), JSON.stringify({ bundle: { externalBin: ["binaries/engine", "binaries/helper"] } }));
  fs.writeFileSync(path.join(f.tauriDir, "binaries", "helper-x86_64-pc-windows-msvc.exe"), "MZ-helper");
  const result = run(f, deps(f), { descriptor: { ...DESCRIPTOR, sidecars: [] } });
  assert.deepEqual(result.sidecars.map((item) => item.name), ["engine", "helper"]);
  const g = fixture(t);
  fs.writeFileSync(path.join(g.tauriDir, "binaries", "svc-x86_64-pc-windows-msvc.exe"), "MZ-svc");
  const release = { sidecars: [{ kind: "cargo-bin", package: "svc" }, { kind: "cargo-bin", package: "macsvc", platforms: ["mac"] }] };
  const out = run(g, deps(g), { descriptor: { ...DESCRIPTOR, sidecars: [] }, releaseConfig: release });
  assert.deepEqual(out.sidecars.map((item) => item.name), ["engine", "svc"]);
});

test("install.sidecars must equal the derived set: a derived sidecar the list leaves out, and a listed sidecar that is not derived, are each refused", (t) => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.tauriDir, "tauri.conf.json"), JSON.stringify({ bundle: { externalBin: ["binaries/engine", "binaries/helper"] } }));
  const d = deps(f);
  assert.equal(codeOf(() => run(f, d)), "TAURI_AUTHENTICODE_SIDECAR_UNLISTED");
  assert.deepEqual(d.log, [], "nothing was signed");
  const g = fixture(t);
  const e = deps(g);
  assert.equal(codeOf(() => run(g, e, { descriptor: { ...DESCRIPTOR, sidecars: ["engine.exe", "extra.exe"] } })), "TAURI_AUTHENTICODE_SIDECAR_UNDERIVED");
  assert.deepEqual(e.log, []);
  // An explicit list that equals the derived set is accepted, not refused as a duplicate.
  const h = fixture(t);
  assert.doesNotThrow(() => run(h, deps(h)));
});

test("refusals: a missing, unsigned or stale engine is refused at each stage with its typed code", (t) => {
  const f = fixture(t);
  fs.rmSync(path.join(f.debugDir, "engine.exe")); fs.rmSync(f.bundled);
  assert.equal(codeOf(() => run(f, deps(f))), "TAURI_AUTHENTICODE_SIDECAR_MISSING");
  // Unsigned: the signer reports an engine that is not Valid.
  const g = fixture(t); const dg = deps(g);
  const sign = dg.deps.signWindows;
  dg.deps.signWindows = (files) => sign(files).map((item) => (path.basename(item.file) === "engine.exe" ? { ...item, authenticode: "NotSigned" } : item));
  assert.equal(codeOf(() => run(g, dg)), "TAURI_AUTHENTICODE_SIDECAR_UNSIGNED");
  // Stale after signing: something rewrote the engine between the signer and the bundler.
  const h = fixture(t); const dh = deps(h);
  const bundle = dh.deps.bundle;
  dh.deps.bundle = (command) => { fs.writeFileSync(h.bundled, "MZ-recopied-unsigned"); bundle(command); };
  assert.equal(codeOf(() => run(h, dh)), "TAURI_AUTHENTICODE_SIDECAR_STALE");
  // Stale in the payload: the installer embeds bytes that are neither signed copy.
  const i = fixture(t); const di = deps(i, { engineFrom: "stale" });
  assert.equal(codeOf(() => run(i, di)), "TAURI_AUTHENTICODE_SIDECAR_STALE");
  assert.equal(di.log.some((line) => line.startsWith("install:")), false);
  // Absent from the payload.
  const j = fixture(t); const dj = deps(j);
  const bundleJ = dj.deps.bundle;
  dj.deps.bundle = (command) => { bundleJ(command); delete j.payload["engine.exe"]; };
  assert.equal(codeOf(() => run(j, dj)), "TAURI_AUTHENTICODE_PAYLOAD");
  // Installed: missing, stale (hash differs), unsigned.
  const k = fixture(t);
  assert.equal(codeOf(() => run(k, deps(k, { installEngine: "missing" }))), "TAURI_AUTHENTICODE_SIDECAR_MISSING");
  const l = fixture(t);
  assert.equal(codeOf(() => run(l, deps(l, { installEngine: "stale" }))), "TAURI_AUTHENTICODE_SIDECAR_STALE");
  const m = fixture(t);
  assert.equal(codeOf(() => run(m, deps(m, { engineAuthenticode: "NotSigned" }))), "TAURI_AUTHENTICODE_SIDECAR_UNSIGNED");
});

test("the payload may match either signed copy (build output or src-tauri/binaries); neither matching is refused", (t) => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.debugDir, "engine.exe"), "MZ-engine-built-differently");
  const d = deps(f);
  const result = run(f, d);
  const copies = result.sidecars[0].copies;
  assert.notEqual(copies[0].sha256After, copies[1].sha256After);
  const g = fixture(t);
  fs.writeFileSync(path.join(g.debugDir, "engine.exe"), "MZ-engine-built-differently");
  assert.doesNotThrow(() => run(g, deps(g, { engineFrom: "debug" })));
  const h = fixture(t);
  fs.writeFileSync(path.join(h.debugDir, "engine.exe"), "MZ-engine-built-differently");
  assert.equal(codeOf(() => run(h, deps(h, { engineFrom: "stale" }))), "TAURI_AUTHENTICODE_SIDECAR_STALE");
});

test("an extra executable in the payload that is not signed here is refused; plugin helpers and the uninstaller are not", (t) => {
  const f = fixture(t); f.extras = ["resources/runtime/ffmpeg.exe"];
  const d = deps(f);
  assert.equal(codeOf(() => run(f, d)), "TAURI_AUTHENTICODE_PAYLOAD_UNSIGNED_EXECUTABLE");
  assert.equal(d.log.some((line) => line.startsWith("install:")), false);
  const g = fixture(t); g.extras = ["uninstall.exe", "resources/runtime/onnxruntime.dll"];
  assert.doesNotThrow(() => run(g, deps(g)));
});

test("the Tauri configuration must exist: no tauri.conf.json means the sidecar set cannot be derived", (t) => {
  const f = fixture(t);
  fs.rmSync(path.join(f.tauriDir, "tauri.conf.json"));
  assert.equal(codeOf(() => run(f, deps(f))), "TAURI_AUTHENTICODE_CONFIG");
});

// Resource executables (bundle.resources) carry the product identity like every other executable the installer wraps (release: win-package-recipe sign-raw).
function withResources(f) {
  fs.mkdirSync(path.join(f.tauriDir, "resources", "runtime"), { recursive: true });
  fs.mkdirSync(path.join(f.tauriDir, "resources", "vad"), { recursive: true });
  fs.writeFileSync(path.join(f.tauriDir, "resources", "runtime", "ffmpeg.exe"), "MZ-unsigned-ffmpeg");
  fs.writeFileSync(path.join(f.tauriDir, "resources", "runtime", "onnxruntime.dll"), "MZ-dll-not-an-exe");
  fs.writeFileSync(path.join(f.tauriDir, "resources", "vad", "vad-tool.exe"), "MZ-unsigned-vad");
  fs.writeFileSync(path.join(f.tauriDir, "resources", "vad", "model.onnx"), "data");
  fs.writeFileSync(path.join(f.tauriDir, "tauri.conf.json"), JSON.stringify({ bundle: { externalBin: ["binaries/engine"], resources: { "resources/runtime/ffmpeg.exe": "runtime/ffmpeg.exe", "resources/runtime/onnxruntime.dll": "runtime/onnxruntime.dll" } } }));
  fs.writeFileSync(path.join(f.tauriDir, "tauri.windows.conf.json"), JSON.stringify({ bundle: { resources: { "resources/vad/*": "vad/" } } }));
  return f;
}

test("resources: every existing .exe from bundle.resources (base and windows merged) is derived; dlls and data are not", (t) => {
  const f = withResources(fixture(t));
  const found = deriveResourceExecutables({ tauriDir: f.tauriDir });
  assert.deepEqual(found.map((r) => r.installRel).sort(), ["runtime/ffmpeg.exe", "vad/vad-tool.exe"]);
  assert.deepEqual(deriveResourceExecutables({ tauriDir: path.join(f.dir, "nowhere"), readFile: () => JSON.stringify({ bundle: { resources: ["resources/missing.exe"] } }) }), [], "a resource that does not exist is not derived");
  const g = fixture(t); fs.mkdirSync(path.join(g.tauriDir, "res", "sub"), { recursive: true }); fs.writeFileSync(path.join(g.tauriDir, "res", "sub", "a.exe"), "MZ");
  fs.writeFileSync(path.join(g.tauriDir, "tauri.conf.json"), JSON.stringify({ bundle: { resources: ["res"] } }));
  assert.deepEqual(deriveResourceExecutables({ tauriDir: g.tauriDir }).map((r) => r.installRel), ["res/sub/a.exe"]);
});

test("resources: each resource exe is signed, found in the payload by its signed digest, and verified installed", (t) => {
  const f = withResources(fixture(t));
  const d = deps(f);
  const result = run(f, d);
  const names = result.sidecars.map((item) => item.name);
  assert.ok(names.includes("runtime/ffmpeg.exe") && names.includes("vad/vad-tool.exe"));
  assert.ok(d.log.some((line) => line.startsWith("sign:") && line.includes("ffmpeg.exe") && line.includes("vad-tool.exe")));
  const ffmpeg = result.sidecars.find((item) => item.name === "runtime/ffmpeg.exe");
  assert.equal(ffmpeg.installed.sha256, ffmpeg.copies[0].sha256After);
  assert.ok(fs.readFileSync(path.join(f.installDir, "runtime", "ffmpeg.exe"), "utf8").endsWith("|AUTHENTICODE"));
  // The unsigned-extra-executable check no longer fires for them: they are signed here.
  assert.equal(result.payload.sidecars.length, result.sidecars.length);
});

test("resources: a stale, unsigned or missing resource exe is refused with the sidecar codes", (t) => {
  const f = withResources(fixture(t));
  assert.equal(codeOf(() => run(f, deps(f, { installResource: "stale" }))), "TAURI_AUTHENTICODE_SIDECAR_STALE");
  const g = withResources(fixture(t)); const dg = deps(g);
  const sign = dg.deps.signWindows;
  dg.deps.signWindows = (files) => sign(files).map((item) => (path.basename(item.file) === "ffmpeg.exe" ? { ...item, authenticode: "NotSigned" } : item));
  assert.equal(codeOf(() => run(g, dg)), "TAURI_AUTHENTICODE_SIDECAR_UNSIGNED");
  const h = withResources(fixture(t)); const dh = deps(h);
  const bundle = dh.deps.bundle;
  dh.deps.bundle = (command) => { bundle(command); h.payload["ffmpeg.exe"] = Buffer.from("MZ-swapped"); };
  assert.equal(codeOf(() => run(h, dh)), "TAURI_AUTHENTICODE_SIDECAR_STALE");
  const i = withResources(fixture(t)); const di = deps(i);
  const install = di.deps.install;
  di.deps.install = (installer) => { install(installer); fs.rmSync(path.join(i.installDir, "runtime", "ffmpeg.exe")); };
  assert.equal(codeOf(() => run(i, di)), "TAURI_AUTHENTICODE_SIDECAR_MISSING");
  const j = withResources(fixture(t)); j.extras = ["resources/unknown-tool.exe"];
  assert.equal(codeOf(() => run(j, deps(j))), "TAURI_AUTHENTICODE_PAYLOAD_UNSIGNED_EXECUTABLE");
});

// In-place signing must not touch the commit: a tracked resource or sidecar copy is refused before anything is signed.
test("a tracked resource exe is refused with TAURI_AUTHENTICODE_TRACKED_RESOURCE and nothing is signed", (t) => {
  const f = withResources(fixture(t));
  const d = deps(f, { staged: { "ffmpeg.exe": "tracked" } });
  assert.equal(codeOf(() => run(f, d)), "TAURI_AUTHENTICODE_TRACKED_RESOURCE");
  assert.equal(d.log.some((line) => line.startsWith("sign:") || line.startsWith("patch:") || line === "bundle"), false);
  const g = fixture(t);
  const e = deps(g, { staged: { "engine-x86_64-pc-windows-msvc.exe": "tracked" } });
  assert.equal(codeOf(() => run(g, e)), "TAURI_AUTHENTICODE_TRACKED_RESOURCE", "a tracked bundler-source sidecar copy is refused the same way");
  const h = withResources(fixture(t));
  assert.equal(codeOf(() => run(h, deps(h, { staged: { "ffmpeg.exe": "unignored" } }))), "TAURI_AUTHENTICODE_TRACKED_RESOURCE", "neither tracked nor ignored cannot be a ledger entry");
});

test("a git-ignored resource is signed in place and reported as a stage output for the ledger", (t) => {
  const f = withResources(fixture(t));
  const result = run(f, deps(f));
  const paths = result.stageOutputs.files.map((file) => path.basename(file.path)).sort();
  assert.deepEqual(paths, ["engine-x86_64-pc-windows-msvc.exe", "ffmpeg.exe", "vad-tool.exe"]);
  assert.ok(result.stageOutputs.files.every((file) => file.kind === "signed-in-place" && /^[0-9a-f]{64}$/.test(file.sha256)));
  assert.ok(!paths.includes("engine.exe") && !paths.includes("rightapp.exe"), "the build output (target/) is not a ledger entry");
});

test("classifyStagedFiles: real git decides tracked, ignored and unignored", (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-classify-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const git = (...args) => spawnSync("git", ["-C", dir, "-c", "user.email=t@t", "-c", "user.name=t", ...args], { encoding: "utf8" });
  git("init", "-q");
  fs.writeFileSync(path.join(dir, ".gitignore"), "ignored.exe\n");
  for (const name of ["tracked.exe", "ignored.exe", "loose.exe"]) fs.writeFileSync(path.join(dir, name), "MZ");
  git("add", ".gitignore", "tracked.exe");
  git("commit", "-qm", "x");
  const files = ["tracked.exe", "ignored.exe", "loose.exe"].map((name) => path.join(dir, name));
  const kinds = classifyStagedFiles(files, { appRoot: dir, tauriDir: dir });
  assert.deepEqual(files.map((file) => kinds.get(file)), ["tracked", "ignored", "unignored"]);
  assert.equal(codeOf(() => classifyStagedFiles([path.join(os.tmpdir(), "rk-not-a-repo-xyz", "a.exe")], { appRoot: dir, tauriDir: dir })), "TAURI_AUTHENTICODE_TRACKED_RESOURCE", "git cannot answer: refused");
});
