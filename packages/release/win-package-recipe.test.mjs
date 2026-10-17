import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import path from "node:path";
import test from "node:test";
import { WIN_PACKAGE_PHASES, WinPackageConfigError, WinPackageRecipeError, runWinPackageRecipe, validateWinPackage } from "./win-package-recipe.mjs";

// Fakes only: no signtool, no makensis, no tauri, no real file system, explicit env objects.
const appRoot = path.resolve("/rk-fake-app");
const at = (...parts) => path.join(appRoot, ...parts);
const RAW = "target/x86_64-pc-windows-msvc/release/demo.exe";
const INSTALLER = "target/x86_64-pc-windows-msvc/release/bundle/nsis/Demo_1.0.0_x64-setup.exe";
const SIDECAR = "resources/helper.exe";
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");

function baseTarget(overrides = {}) {
  return {
    signingContract: "windows-raw-exe-authenticode-before-nsis-v1",
    sign: { prePackageFiles: [RAW], files: [INSTALLER] },
    updater: { keyFile: "%APPDATA%\\ViewRight\\updater.key" },
    winPackage: { installer: "nsis", productName: "Demo", updater: true, sidecars: [SIDECAR] },
    ...overrides,
  };
}

function world({ fail = {}, mutateRawDuringBundle = false, skipSig = false } = {}) {
  const files = new Map([[at(...SIDECAR.split("/")), Buffer.from("sidecar")]]);
  const events = [];
  const env = { RIGHT_TEST_ENV: "explicit", PATH: "fake" };
  const key = (file) => path.resolve(file);
  const fs = {
    existsSync: (file) => files.has(key(file)),
    readFileSync: (file) => {
      if (!files.has(key(file))) throw new Error(`ENOENT ${file}`);
      return files.get(key(file));
    },
    rmSync: (file) => { files.delete(key(file)); },
    mkdirSync: () => {},
  };
  const deps = {
    env,
    platform: "win32",
    log: () => {},
    fs,
    spawn(command, args, options) {
      const verb = args[2];
      events.push({ step: `tauri-${verb}`, command, args, options });
      if (fail[verb]) return { status: 1 };
      if (verb === "build") files.set(at(...RAW.split("/")), Buffer.from("raw __TAURI_BUNDLE_TYPE_VAR_UNK"));
      if (verb === "bundle") {
        if (mutateRawDuringBundle) files.set(at(...RAW.split("/")), Buffer.from("rewritten by bundler"));
        files.set(at(...INSTALLER.split("/")), Buffer.from("installer"));
      }
      return { status: 0 };
    },
    patchMarker(file, options) {
      events.push({ step: "marker", file, options });
      if (fail.marker) throw new Error("marker ambiguous");
      files.set(key(file), Buffer.from("raw __TAURI_BUNDLE_TYPE_VAR_NSS"));
      return { file, bundle: options.bundle, changedBytes: 2, alreadyPatched: false };
    },
    signWindows(list, options) {
      events.push({ step: `sign-${options.phase}`, files: list.map(key), present: list.map((file) => files.has(key(file))) });
      if (options.dryRun) return [];
      if (fail[options.phase]) throw new Error("signtool failed");
      return list.map((file) => {
        files.set(key(file), Buffer.concat([files.get(key(file)), Buffer.from("|signed")]));
        return { file: key(file), sha256: sha(files.get(key(file))) };
      });
    },
    verifyEmbedded(options) {
      events.push({ step: "verify-embedded", options });
      if (fail.embedded) throw new Error("installer does not embed the signed demo.exe");
      return { entry: options.entryName, sha256: options.expectedSha256 };
    },
    signUpdater(file, options) {
      events.push({ step: "sign-updater", file: key(file), options });
      if (fail.updater) throw new Error("sign-updater exited 1");
      if (!skipSig) files.set(`${key(file)}.sig`, Buffer.from("sig"));
    },
  };
  return { deps, events, files, env };
}

const run = (target, w) => runWinPackageRecipe({ appRoot, config: {}, target, deps: w.deps });
const steps = (w) => w.events.map((event) => event.step);

test("a complete config validates and resolves every path under the app root", () => {
  const plan = validateWinPackage({ appRoot, config: {}, target: baseTarget() });
  assert.equal(plan.rawExe, at(...RAW.split("/")));
  assert.equal(plan.installerFile, at(...INSTALLER.split("/")));
  assert.deepEqual([...plan.sidecars], [at("resources", "helper.exe")]);
  assert.equal(plan.perMachine, false);
  assert.equal(plan.triple, "x86_64-pc-windows-msvc");
  assert.equal(plan.updaterKeyFile, "%APPDATA%\\ViewRight\\updater.key");
});

test("invalid configuration throws a typed error naming every problem", () => {
  const bad = (winPackage, extra = {}) => {
    const target = baseTarget({ winPackage, ...extra });
    try { validateWinPackage({ appRoot, config: {}, target }); } catch (error) { return error; }
    assert.fail("expected WinPackageConfigError");
  };
  const good = baseTarget().winPackage;
  const cases = [
    [{ ...good, installer: "msi" }, /installer must be "nsis"/],
    [{ ...good, productName: "" }, /productName/],
    [{ ...good, productName: "a/b" }, /productName/],
    [{ ...good, updater: undefined }, /updater must be true or false/],
    [{ ...good, updater: "yes" }, /updater must be true or false/],
    [{ ...good, perMachine: "no" }, /perMachine/],
    [{ ...good, extra: 1 }, /unknown winPackage key: extra/],
    [{ ...good, sidecars: "x.exe" }, /sidecars must be an array/],
    [{ ...good, sidecars: ["C:\\abs\\x.exe"] }, /relative to the app root/],
    [{ ...good, sidecars: ["../outside.exe"] }, /stay inside the app root/],
    [{ ...good, sidecars: ["a.exe", "A.exe"] }, /duplicates/],
    [{ ...good, sidecars: ["notes.txt"] }, /\.exe or \.dll/],
    [{ ...good, sidecars: [RAW] }, /is the raw EXE/],
    [{ ...good, triple: "aarch64-apple-darwin" }, /Windows target triple/],
    [{ ...good, rawExe: "demo.bin" }, /rawExe must be an \.exe/],
  ];
  for (const [spec, pattern] of cases) {
    const error = bad(spec);
    assert.ok(error instanceof WinPackageConfigError, String(pattern));
    assert.equal(error.code, "RK_WIN_PACKAGE_CONFIG");
    assert.match(error.message, pattern);
  }
  assert.match(bad(good, { package: { cmd: "node" } }).message, /mutually exclusive/);
  assert.match(bad(good, { sign: {} }).message, /raw EXE path is unknown/);
  assert.match(bad(good, { sign: { prePackageFiles: [RAW] } }).message, /installer path is unknown/);
  const several = bad({ ...good, installer: "msi", productName: "", updater: 3 });
  assert.equal(several.issues.length, 3);
  assert.throws(() => validateWinPackage({ appRoot, config: { packageManager: "npm" }, target: baseTarget() }), /pnpm/);
  assert.throws(() => validateWinPackage({ appRoot, config: {}, target: { sign: {} } }), WinPackageConfigError);
});

test("an invalid config performs no effect at all", () => {
  const w = world();
  assert.throws(() => run(baseTarget({ winPackage: { ...baseTarget().winPackage, installer: "msi" } }), w), WinPackageConfigError);
  assert.deepEqual(w.events, []);
});

test("phases run in the declared order and the receipt records them", () => {
  const w = world();
  const receipt = run(baseTarget(), w);
  assert.deepEqual(steps(w), ["tauri-build", "marker", "sign-raw-exe", "tauri-bundle", "verify-embedded", "sign-installer", "sign-updater"]);
  assert.deepEqual(receipt.phases, [...WIN_PACKAGE_PHASES]);
  assert.equal(receipt.schema, 1);
  assert.equal(receipt.productName, "Demo");
  assert.match(receipt.rawExe.signedSha256, /^[0-9a-f]{64}$/);
  assert.match(receipt.installer.signedSha256, /^[0-9a-f]{64}$/);
  assert.equal(receipt.updaterSignature, `${at(...INSTALLER.split("/"))}.sig`);
});

test("the bundle marker is patched to nsis on the raw EXE before anything is signed", () => {
  const w = world();
  run(baseTarget(), w);
  const marker = w.events.find((event) => event.step === "marker");
  assert.equal(marker.file, at(...RAW.split("/")));
  assert.deepEqual(marker.options, { bundle: "nsis" });
  assert.ok(steps(w).indexOf("marker") < steps(w).indexOf("sign-raw-exe"));
});

test("every executable the installer wraps is signed, and present, before the installer is bundled", () => {
  const w = world();
  run(baseTarget(), w);
  const order = steps(w);
  const sign = w.events.find((event) => event.step === "sign-raw-exe");
  assert.deepEqual(sign.files, [at("resources", "helper.exe"), at(...RAW.split("/"))]);
  assert.deepEqual(sign.present, [true, true]);
  assert.ok(order.indexOf("sign-raw-exe") < order.indexOf("tauri-bundle"));
  assert.ok(order.indexOf("tauri-bundle") < order.indexOf("sign-installer"));
  assert.equal(w.events.find((event) => event.step === "sign-installer").files.length, 1);
});

test("the updater signature is made after the installer is signed, over the installer", () => {
  const w = world();
  run(baseTarget(), w);
  const order = steps(w);
  assert.ok(order.indexOf("sign-updater") > order.indexOf("sign-installer"));
  const call = w.events.find((event) => event.step === "sign-updater");
  assert.equal(call.file, at(...INSTALLER.split("/")));
  assert.equal(call.options.keyFile, "%APPDATA%\\ViewRight\\updater.key");
});

test("updater:false skips the updater phase and perMachine selects the install mode", () => {
  const w = world();
  const receipt = run(baseTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: false, perMachine: true } }), w);
  assert.ok(!steps(w).includes("sign-updater"));
  assert.ok(!receipt.phases.includes("updater"));
  assert.equal(receipt.updaterSignature, null);
  const bundle = w.events.find((event) => event.step === "tauri-bundle");
  const override = JSON.parse(bundle.args[bundle.args.indexOf("--config") + 1]);
  assert.equal(override.bundle.createUpdaterArtifacts, false);
  assert.equal(override.bundle.windows.nsis.installMode, "perMachine");
});

test("spawns use the explicit env, an empty Tauri key and the declared triple", () => {
  const w = world();
  run(baseTarget(), w);
  const build = w.events.find((event) => event.step === "tauri-build");
  assert.deepEqual(build.args.slice(0, 5), ["exec", "tauri", "build", "--no-bundle", "--target"]);
  assert.equal(build.args[5], "x86_64-pc-windows-msvc");
  assert.equal(build.command, "pnpm");
  assert.equal(build.options.cwd, appRoot);
  assert.equal(build.options.env.RIGHT_TEST_ENV, "explicit");
  assert.equal(build.options.env.TAURI_SIGNING_PRIVATE_KEY, "");
  const bundle = w.events.find((event) => event.step === "tauri-bundle");
  assert.deepEqual(bundle.args.slice(0, 4), ["exec", "tauri", "bundle", "--bundles"]);
  assert.equal(bundle.args[4], "nsis");
});

test("a failed build stops the recipe before marker, signing and bundling", () => {
  const w = world({ fail: { build: true } });
  assert.throws(() => run(baseTarget(), w), (error) => error instanceof WinPackageRecipeError && error.phase === "build" && error.code === "RK_WIN_PACKAGE_PHASE");
  assert.deepEqual(steps(w), ["tauri-build"]);
});

test("a marker failure stops the recipe before any signing", () => {
  const w = world({ fail: { marker: true } });
  assert.throws(() => run(baseTarget(), w), (error) => error.phase === "marker" && /marker ambiguous/.test(error.message) && error.cause instanceof Error);
  assert.ok(!steps(w).includes("sign-raw-exe"));
});

test("a signing failure never reaches the bundle step", () => {
  const w = world({ fail: { "raw-exe": true } });
  assert.throws(() => run(baseTarget(), w), (error) => error.phase === "sign-raw");
  assert.ok(!steps(w).includes("tauri-bundle"));
});

test("a failed bundle stops before verification and installer signing", () => {
  const w = world({ fail: { bundle: true } });
  assert.throws(() => run(baseTarget(), w), (error) => error.phase === "bundle");
  assert.ok(!steps(w).includes("sign-installer"));
});

test("a signed raw EXE rewritten by the bundler is refused before the installer is signed", () => {
  const w = world({ mutateRawDuringBundle: true });
  assert.throws(() => run(baseTarget(), w), (error) => error.phase === "verify" && /rewrote the signed raw EXE/.test(error.message));
  assert.ok(!steps(w).includes("verify-embedded"));
  assert.ok(!steps(w).includes("sign-installer"));
});

test("an installer that does not embed the signed EXE is refused before it is signed", () => {
  const w = world({ fail: { embedded: true } });
  assert.throws(() => run(baseTarget(), w), (error) => error.phase === "verify");
  assert.ok(!steps(w).includes("sign-installer"));
  assert.ok(!steps(w).includes("sign-updater"));
});

test("the verification receives the digest that signing recorded", () => {
  const w = world();
  const receipt = run(baseTarget(), w);
  const verify = w.events.find((event) => event.step === "verify-embedded").options;
  assert.equal(verify.expectedSha256, receipt.rawExe.signedSha256);
  assert.equal(verify.entryName, "demo.exe");
  assert.equal(verify.installer, at(...INSTALLER.split("/")));
});

test("an installer signing failure never produces an updater signature", () => {
  const w = world({ fail: { installer: true } });
  assert.throws(() => run(baseTarget(), w), (error) => error.phase === "sign-installer");
  assert.ok(!steps(w).includes("sign-updater"));
});

test("an updater failure and a missing signature file both fail the updater phase", () => {
  assert.throws(() => run(baseTarget(), world({ fail: { updater: true } })), (error) => error.phase === "updater");
  assert.throws(() => run(baseTarget(), world({ skipSig: true })), (error) => error.phase === "updater" && /did not write/.test(error.message));
});

test("signing evidence missing a digest is a failure, not a pass", () => {
  const w = world();
  w.deps.signWindows = () => [];
  assert.throws(() => run(baseTarget(), w), (error) => error.phase === "sign-raw" && /no signed digest/.test(error.message));
});

test("a raw EXE the build did not produce, or a missing sidecar, fails the build phase", () => {
  const noRaw = world();
  const spawn = noRaw.deps.spawn;
  noRaw.deps.spawn = (command, args, options) => { spawn(command, args, options); noRaw.files.delete(at(...RAW.split("/"))); return { status: 0 }; };
  assert.throws(() => run(baseTarget(), noRaw), (error) => error.phase === "build" && /did not produce the raw EXE/.test(error.message));
  const noSidecar = world();
  noSidecar.files.delete(at("resources", "helper.exe"));
  assert.throws(() => run(baseTarget(), noSidecar), (error) => error.phase === "build" && /sidecar does not exist/.test(error.message));
  assert.ok(!steps(noSidecar).includes("sign-raw-exe"));
});

test("the recipe refuses to run off Windows, and a dry run performs no signing effects", () => {
  const off = world();
  off.deps.platform = "darwin";
  assert.throws(() => run(baseTarget(), off), (error) => error.phase === "build" && /must run on Windows/.test(error.message));
  assert.deepEqual(off.events, []);
  const dry = world();
  dry.deps.dryRun = true;
  dry.deps.platform = "darwin";
  const receipt = run(baseTarget(), dry);
  assert.equal(receipt.dryRun, true);
  assert.ok(!steps(dry).some((step) => step.startsWith("tauri-")));
  assert.ok(!steps(dry).includes("marker"));
  assert.ok(!steps(dry).includes("verify-embedded"));
  assert.equal(receipt.rawExe.signedSha256, null);
});
