import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import { BUILD_LAUNCH_REFUSED, buildLaunch, cmakeRefusal, lockedNdkArgs, pnpmTailRefusal } from "./build-launch.mjs";
import { SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { CARRIED_CARGO_MAP_ENV } from "./carried-cargo.mjs";
import { PNPM_ADAPTER_GRAMMAR } from "./pnpm-adapter-grammar.mjs";

const CLIENT = "/managed/agent-bin/rustright";
const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
const direct = { env: { PATH: "/usr/bin" }, platform: "linux", exists: () => false, which: () => [] };
// Broker-managed (a RustRight data root exists) but the client is unreachable from this shell.
const unreachable = { env: { PATH: "/usr/bin", RIGHTKIT_BUILD_BROKER_SOCKET: "/x/broker.sock" }, platform: "darwin", exists: () => true, which: () => [], readFile: () => "" };
const ownRightRelease = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "cli", "right-release.mjs");

test("managed host: pnpm is a rightkit-pnpm broker job; cargo and rustright go by the broker client; the door's own right-release is unchanged", () => {
  assert.deepEqual(buildLaunch("pnpm", ["--dir", "apps/web", "run", "build"], managed), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "--dir", "apps/web", "run", "build"] });
  assert.deepEqual(buildLaunch("pnpm.cmd", ["run", "test"], managed).args, ["run", "rightkit-pnpm", "--", "run", "test"]);
  assert.deepEqual(buildLaunch("cargo", ["build"], managed), { command: CLIENT, args: ["cargo", "build"] });
  assert.deepEqual(buildLaunch("swift", ["build", "--package-path", "swift/helper"], managed), { command: CLIENT, args: ["run", "rightkit-swift", "--", "build", "--package-path", "swift/helper"] });
  // A real cargo named by absolute path is still a broker job: the path is not trusted, the name is routed.
  assert.deepEqual(buildLaunch("/Users/x/.cargo/bin/cargo", ["build"], managed), { command: CLIENT, args: ["cargo", "build"] });
  assert.deepEqual(buildLaunch("/elsewhere/rustright", ["cargo", "test"], managed), { command: CLIENT, args: ["cargo", "test"] });
  assert.deepEqual(buildLaunch(process.execPath, [ownRightRelease, "web-build"], managed), { command: process.execPath, args: [ownRightRelease, "web-build"] });
});

test("managed host: node runs a workspace script as a rightkit-node broker job; a rightkit-qa run is a rightkit-qa broker job", () => {
  const host = { ...managed, cwd: "/w/app" };
  assert.deepEqual(buildLaunch("node", ["node_modules/vite/bin/vite.js", "build"], host), { command: CLIENT, args: ["run", "rightkit-node", "--", "node_modules/vite/bin/vite.js", "build"] });
  assert.deepEqual(buildLaunch(process.execPath, ["/w/app/scripts/dev-build.mjs", "--platform", "mac"], host).args, ["run", "rightkit-node", "--", "scripts/dev-build.mjs", "--platform", "mac"]);
  // The whole QA run (its test binaries included) is one broker job.
  assert.deepEqual(buildLaunch("rightkit-qa", ["run-tests", "--app", "heardright"], host), { command: CLIENT, args: ["run", "rightkit-qa", "--", "run-tests", "--app", "heardright"] });
});

test("managed host: any other build or test command is refused, never run around the broker", () => {
  // An executable merely named node that names right-release is not this Node binary running this package's right-release.
  assert.throws(() => buildLaunch("/tmp/evil/node", [ownRightRelease, "web-build"], { ...managed, cwd: "/w/app" }), (e) => e.code === BUILD_LAUNCH_REFUSED);
  for (const [command, args] of [["vite", ["build"]], ["npm", ["run", "build"]], ["sh", ["scripts/prepackage-mac.sh"]], ["gradle", ["assembleDebug"]], ["sh", ["-c", "pnpm run build"]], ["tsc", ["-p", "."]], ["node", ["/elsewhere/build.mjs"]], ["node", ["../other/build.mjs"]], ["node", ["-e", "1"]], ["powershell", ["-File", "x.ps1"]]]) {
    assert.throws(() => buildLaunch(command, args, managed), (e) => e.code === BUILD_LAUNCH_REFUSED && /rightkit#1241/.test(e.message), command);
  }
});

test("host detection reads the cargo on PATH: the RightKit cargo guard makes it managed (pnpm then needs the client); a plain cargo is direct", () => {
  // Mac and Windows only: every Linux host is direct whatever cargo is on PATH (gate-executor directPlatform).
  const guard = { env: { PATH: "/g" }, platform: "darwin", exists: () => false, which: (c) => (c === "cargo" ? ["/g/cargo"] : []), readFile: () => "#!/usr/bin/env node\nimport './cargo-guard.mjs';\n" };
  assert.throws(() => buildLaunch("pnpm", ["run", "build"], guard), (e) => e.code === BUILD_LAUNCH_REFUSED && /rustright is not on PATH/.test(e.message));
  const plain = { ...guard, readFile: () => "\x7fELF" };
  assert.deepEqual(buildLaunch("pnpm", ["run", "build"], plain), { command: "pnpm", args: ["run", "build"] });
  assert.equal(buildLaunch("pnpm", ["run", "build"], { ...guard, platform: "linux" }).command, "pnpm");
});

test("a managed host whose broker is unreachable refuses every launch; a host without a broker runs directly", () => {
  for (const [command, args] of [["pnpm", ["run", "build"]], ["cargo", ["build"]], ["node", ["scripts/build.mjs"]], ["rightkit-qa", ["run-tests"]]]) {
    assert.throws(() => buildLaunch(command, args, unreachable), (e) => e.code === BUILD_LAUNCH_REFUSED && /broker-managed/.test(e.message), command);
  }
  assert.deepEqual(buildLaunch("pnpm", ["run", "build"], direct), { command: "pnpm", args: ["run", "build"] });
  assert.deepEqual(buildLaunch("vite", ["build"], direct), { command: "vite", args: ["build"] });
});

test("managed host: node flags that change no code path pass to the adapter; eval, import, require and loader flags refuse", () => {
  const host = { ...managed, cwd: "/w/app" };
  assert.deepEqual(buildLaunch("node", ["--disable-warning=DEP0205", "node_modules/typescript/bin/tsc"], host).args, ["run", "rightkit-node", "--", "--disable-warning=DEP0205", "node_modules/typescript/bin/tsc"]);
  for (const flags of [["-e", "1"], ["--eval=1"], ["-p", "1"], ["--import", "x.mjs"], ["--import=x.mjs"], ["-r", "x"], ["--require=x"], ["--loader", "x"], ["--experimental-loader=x"]]) {
    assert.throws(() => buildLaunch("node", [...flags, "scripts/build.mjs"], host), (e) => e.code === BUILD_LAUNCH_REFUSED, flags.join(" "));
  }
});

test("managed host: npm (whose install lifecycle scripts can build) refuses; the door's own right-release refuses added startup code", () => {
  assert.throws(() => buildLaunch("npm", ["ci"], managed), (e) => e.code === BUILD_LAUNCH_REFUSED);
  const injected = { ...managed, env: { ...managed.env, NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --import=/tmp/build.mjs`.trim() } };
  assert.throws(() => buildLaunch(process.execPath, [ownRightRelease, "web-build"], injected), (e) => e.code === BUILD_LAUNCH_REFUSED && /NODE_OPTIONS/.test(e.message));
  // Reordered tokens of an inherited value can re-pair an option with another value: anything but the exact string refuses.
  const original = process.env.NODE_OPTIONS;
  process.env.NODE_OPTIONS = "--max-old-space-size=4096 --import ./a.mjs";
  try {
    const reordered = { ...managed, env: { ...managed.env, NODE_OPTIONS: "--import --max-old-space-size=4096 ./a.mjs" } };
    assert.throws(() => buildLaunch(process.execPath, [ownRightRelease, "web-build"], reordered), (e) => e.code === BUILD_LAUNCH_REFUSED);
    const exact = { ...managed, env: { ...managed.env, NODE_OPTIONS: process.env.NODE_OPTIONS } };
    assert.equal(buildLaunch(process.execPath, [ownRightRelease, "web-build"], exact).command, process.execPath);
  } finally { if (original === undefined) delete process.env.NODE_OPTIONS; else process.env.NODE_OPTIONS = original; }
  const same = { ...managed, env: { ...managed.env, ...(process.env.NODE_OPTIONS !== undefined ? { NODE_OPTIONS: process.env.NODE_OPTIONS } : {}) } };
  assert.deepEqual(buildLaunch(process.execPath, [ownRightRelease, "web-build"], same).command, process.execPath);
});

// ---- GATE-5 (gate-vs-broker audit 2026-10-30): B5/B6, M4, M12, M13, M28, M31 ----------------------------------------------------------------

const releaseDir = path.dirname(fileURLToPath(import.meta.url));
const adapterRun = path.join(releaseDir, "carried-adapter-run.mjs");
const cargoRun = path.join(releaseDir, "carried-cargo-run.mjs");
const pinned = { pnpmGrammar: PNPM_ADAPTER_GRAMMAR };
/** A host on a snapshot stage: the carrier map file is in the environment. */
function carriedHost(t, extra = {}) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-bl-carrier-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const mapFile = path.join(dir, "map.json");
  writeFileSync(mapFile, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", broker: { checkout: "/primary", snapshot: { path: "/snap" } }, ledgerDir: dir, workspaces: {} }));
  return { ...managed, ...pinned, env: { ...managed.env, [CARRIED_CARGO_MAP_ENV]: mapFile }, cwd: "/snap/app", ...extra };
}

test("B5/B6: xcodegen, xcodebuild and gradlew are registered adapter jobs (rightkit-xcodegen|rightkit-xcodebuild|rightkit-gradle), never a direct run", () => {
  assert.deepEqual(buildLaunch("xcodegen", ["generate", "--spec", "/w/app/project.yml", "--project", "/w/app/ios"], managed), { command: CLIENT, args: ["run", "rightkit-xcodegen", "--", "generate", "--spec", "/w/app/project.yml", "--project", "/w/app/ios"] });
  const build = ["-project", "/w/app/ios/App.xcodeproj", "-scheme", "App", "-configuration", "Debug", "-destination", "id=ABC", "-derivedDataPath", "/managed/dd", "DEVELOPMENT_TEAM=TEAM123456", "-allowProvisioningUpdates", "build"];
  assert.deepEqual(buildLaunch("xcodebuild", build, managed), { command: CLIENT, args: ["run", "rightkit-xcodebuild", "--", ...build] });
  assert.deepEqual(buildLaunch("./gradlew", [":app:assembleDebug"], managed), { command: CLIENT, args: ["run", "rightkit-gradle", "--", ":app:assembleDebug"] });
  assert.deepEqual(buildLaunch("gradlew.bat", ["bundleRelease", "--no-daemon", "--console=plain"], managed).args, ["run", "rightkit-gradle", "--", "bundleRelease", "--no-daemon", "--console=plain"]);
  // A plain `gradle` (no wrapper) is still no adapter.
  assert.throws(() => buildLaunch("gradle", ["assembleDebug"], managed), (e) => e.code === BUILD_LAUNCH_REFUSED);
  // The argv the adapters would not take is refused at the door.
  for (const [command, args] of [
    ["xcodegen", ["--version"]], ["xcodegen", ["generate", "--use-cache"]],
    ["xcodebuild", ["-runFirstLaunch"]], ["xcodebuild", ["build", "OTHER_LDFLAGS=-fplugin"]], ["xcodebuild", ["-scheme", "App"]], ["xcodebuild", ["-project"]],
    ["./gradlew", ["assembleDebug", "-Pandroid.injected=1"]], ["./gradlew", ["assembleDebug", "--init-script", "x.gradle"]], ["./gradlew", ["--no-daemon"]], ["./gradlew", ["assembleDebug", "--project-dir", ".."]],
  ]) assert.throws(() => buildLaunch(command, args, managed), (e) => e.code === BUILD_LAUNCH_REFUSED, `${command} ${args.join(" ")}`);
});

test("B5/B6: on a snapshot stage the toolchain adapters are carried adapter jobs, and `rustright run <adapter>` of them maps back to the same door rules", (t) => {
  const host = carriedHost(t);
  assert.deepEqual(buildLaunch("xcodebuild", ["-project", "App.xcodeproj", "-scheme", "App", "build"], host).args, [adapterRun, "rightkit-xcodebuild", "--", "-project", "App.xcodeproj", "-scheme", "App", "build"]);
  assert.deepEqual(buildLaunch("./gradlew", [":app:assembleDebug"], host).args, [adapterRun, "rightkit-gradle", "--", ":app:assembleDebug"]);
  assert.equal(buildLaunch("rustright", ["run", "rightkit-gradle", "--", ":app:assembleDebug"], host).args[1], "rightkit-gradle");
  assert.throws(() => buildLaunch("rustright", ["run", "rightkit-gradle", "--", ":app:assembleDebug", "-Pk=v"], host), (e) => e.code === BUILD_LAUNCH_REFUSED);
});

test("M28: cmake is a rightkit-cmake adapter job for the configure and build argv only; -E, -P, --install, --preset and positionals refuse", (t) => {
  const configure = ["-S", "/c/src", "-B", "/c/build", "-DCMAKE_BUILD_TYPE=Release", "-G", "Ninja", "-A", "x64", "-DCMAKE_OSX_ARCHITECTURES=arm64;x86_64"];
  const build = ["--build", "/c/build", "--config", "Release", "--target", "whisper-cli", "--parallel"];
  assert.equal(cmakeRefusal(configure), null);
  assert.equal(cmakeRefusal(build), null);
  assert.equal(cmakeRefusal(["--build", "/c/build", "--parallel", "8"]), null);
  assert.deepEqual(buildLaunch("cmake", configure, managed), { command: CLIENT, args: ["run", "rightkit-cmake", "--", ...configure] });
  assert.deepEqual(buildLaunch("cmake", build, carriedHost(t)).args, [adapterRun, "rightkit-cmake", "--", ...build]);
  for (const bad of [["-E", "remove_directory", "/"], ["-P", "x.cmake"], ["--install", "/c/build"], ["--preset", "x"], ["-C", "init.cmake"], ["--toolchain", "t.cmake"], ["-S", "/c/src", "positional"], ["-B"], []]) {
    assert.notEqual(cmakeRefusal(bad), null, bad.join(" "));
    assert.throws(() => buildLaunch("cmake", bad, managed), (e) => e.code === BUILD_LAUNCH_REFUSED, bad.join(" "));
  }
});

test("M31: a carried `cargo ndk ... build` gets --locked right after cargo-ndk's cargo subcommand, once, and never inside a -- tail", (t) => {
  const ndk = ["ndk", "--platform", "26", "-t", "arm64-v8a", "-t", "x86_64", "-o", "app/src/main/jniLibs", "build", "--release", "--manifest-path", "Cargo.toml"];
  assert.deepEqual(lockedNdkArgs(ndk), ["ndk", "--platform", "26", "-t", "arm64-v8a", "-t", "x86_64", "-o", "app/src/main/jniLibs", "build", "--locked", "--release", "--manifest-path", "Cargo.toml"]);
  assert.deepEqual(lockedNdkArgs(["ndk", "-o", "build", "build"]), ["ndk", "-o", "build", "build", "--locked"], "an output directory named like the subcommand is a value, not the subcommand");
  assert.deepEqual(lockedNdkArgs(["ndk", "-t", "x86_64", "build", "--frozen"]), ["ndk", "-t", "x86_64", "build", "--frozen"]);
  assert.deepEqual(lockedNdkArgs(["ndk", "-t", "x86_64", "build", "--locked"]), ["ndk", "-t", "x86_64", "build", "--locked"]);
  assert.deepEqual(lockedNdkArgs(["ndk", "-t", "x86_64", "build", "--", "--locked"]), ["ndk", "-t", "x86_64", "build", "--locked", "--", "--locked"]);
  assert.deepEqual(lockedNdkArgs(["build", "--release"]), ["build", "--release"], "only `ndk` is touched here");
  const host = carriedHost(t);
  assert.deepEqual(buildLaunch("cargo", ndk, host), { command: process.execPath, args: [cargoRun, ...lockedNdkArgs(ndk)] });
  assert.deepEqual(buildLaunch("rustright", ["cargo", ...ndk], host).args, [cargoRun, ...lockedNdkArgs(ndk)]);
  assert.ok(buildLaunch("cargo", ndk, host).args.includes("--locked"));
});

test("M12: a leading flag the adapter does not register (--if-present, --prefix, --config) and an unregistered global option are refused at the door; --store-dir cannot hide a pnpm exec target", () => {
  const host = { ...managed, ...pinned };
  for (const args of [["--if-present", "run", "build"], ["--prefix", "x", "run", "build"], ["--config", "x", "run", "build"], ["--dir", "apps/web", "--prefix", "x", "run", "build"]]) {
    assert.throws(() => buildLaunch("pnpm", args, host), (e) => e.code === BUILD_LAUNCH_REFUSED && /rightkit-pnpm/.test(e.message), args.join(" "));
  }
  assert.deepEqual(buildLaunch("pnpm", ["--dir", "apps/web", "--filter", "web", "run", "build"], host).args, ["run", "rightkit-pnpm", "--", "--dir", "apps/web", "--filter", "web", "run", "build"]);
  // `--store-dir s` takes a value: `s` is not the command, so `exec node -e` still meets the exec target rule.
  assert.throws(() => buildLaunch("pnpm", ["--dir", "a", "--store-dir", "s", "exec", "node", "-e", "1"], host), (e) => e.code === BUILD_LAUNCH_REFUSED && /pnpm exec target/.test(e.message));
});

test("M13: `pnpm run <script> --flag` with no `--` is refused naming the `--` form; the `--` form, registered flags and exec tails pass", () => {
  const host = { ...managed, ...pinned };
  assert.throws(() => buildLaunch("pnpm", ["run", "build", "--mode", "production"], host), (e) => e.code === BUILD_LAUNCH_REFUSED && /pnpm run <script> -- --mode/.test(e.message));
  assert.match(pnpmTailRefusal(["run", "test", "--watch=false"], PNPM_ADAPTER_GRAMMAR), /pnpm run <script> -- --watch=false/);
  assert.deepEqual(buildLaunch("pnpm", ["run", "build", "--", "--mode", "production"], host).args, ["run", "rightkit-pnpm", "--", "run", "build", "--", "--mode", "production"]);
  assert.equal(pnpmTailRefusal(["run", "build", "--if-present"], PNPM_ADAPTER_GRAMMAR), null, "a registered flag is the broker's flag");
  assert.equal(pnpmTailRefusal(["exec", "tauri", "build", "--target", "x86_64-apple-darwin", "--bundles", "app"], PNPM_ADAPTER_GRAMMAR), null, "after the exec target everything is the target's own");
  assert.equal(pnpmTailRefusal(["--dir", "apps/web", "run", "build"], PNPM_ADAPTER_GRAMMAR), null);
  assert.equal(pnpmTailRefusal(["pack", "--pack-destination", "out"], PNPM_ADAPTER_GRAMMAR), null);
});

test("M4: this package's recipe-build.mjs and create-mac-updater.mjs run in process (never a node adapter job), and TAURI_SIGNING_* never enters a broker request", (t) => {
  const quiet = { TAURI_SIGNING_PRIVATE_KEY: "", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" };
  for (const script of ["recipe-build.mjs", "create-mac-updater.mjs"]) {
    const launch = buildLaunch(process.execPath, [path.join(releaseDir, script), "--plan", "{}"], { ...managed, cwd: "/w/app", stepEnv: quiet });
    assert.deepEqual(launch, { command: process.execPath, args: [path.join(releaseDir, script), "--plan", "{}"] }, `${script} is in process: no rustright, no RUSTRIGHT_ADAPTER_ENV`);
    const carried = buildLaunch(process.execPath, [path.join(releaseDir, script)], { ...carriedHost(t), stepEnv: quiet });
    assert.equal(carried.command, process.execPath);
    assert.ok(!carried.args.includes(adapterRun), `${script} is not a carried adapter job`);
  }
  // A script of another directory named the same is not this package's.
  assert.throws(() => buildLaunch(process.execPath, ["/tmp/evil/recipe-build.mjs"], { ...managed, cwd: "/w/app" }), (e) => e.code === BUILD_LAUNCH_REFUSED);
  // A pnpm step that declares the quiet (empty) signing env sends neither the names nor RUSTRIGHT_ADAPTER_ENV; a step env of other names still rides.
  const quietPnpm = buildLaunch("pnpm", ["exec", "tauri", "build"], { ...carriedHost(t), stepEnv: quiet });
  assert.equal(quietPnpm.env, undefined);
  const mixed = buildLaunch("pnpm", ["exec", "tauri", "build"], { ...carriedHost(t), stepEnv: { ...quiet, RK_ISOLATED_CHECKOUT: "1" } });
  assert.equal(mixed.env.RUSTRIGHT_ADAPTER_ENV, "RK_ISOLATED_CHECKOUT");
  assert.ok(!Object.keys(mixed.env).some((k) => /TAURI_SIGNING/.test(k)));
  // A non-empty signing value is a secret and is refused, never forwarded or echoed.
  assert.throws(() => buildLaunch("pnpm", ["exec", "tauri", "build"], { ...carriedHost(t), stepEnv: { TAURI_SIGNING_PRIVATE_KEY: "s3cr3t-value" } }), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_ADAPTER_ENV_SECRET" && !e.message.includes("s3cr3t-value"));
  assert.throws(() => buildLaunch("pnpm", ["exec", "tauri", "build"], { ...managed, ...pinned, stepEnv: { TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "p" } }), (e) => e.reason === "RK_ADAPTER_ENV_SECRET");
});
