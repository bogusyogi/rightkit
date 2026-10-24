// Fixture apps and spawn helpers shared by the release tests (release.test.mjs, admission.test.mjs, doctor-parity.test.mjs):
// a committed fixture repository with a release config, the doctor entry points, and fake build tools for the host rows.
import { tempRoot } from "./temp-roots.mjs";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import { createHash } from "node:crypto";
import path, { delimiter } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "../background-process.mjs";
import { admittedWorkerEnv, registryEnv } from "./admission.mjs";

export const versions = JSON.parse(readFileSync(new URL("../rightkit-versions.json", import.meta.url), "utf8"));
export const release = fileURLToPath(new URL("../release.mjs", import.meta.url));
export const build = fileURLToPath(new URL("../build-release.mjs", import.meta.url));
// Fixture apps carry the standard Cargo profile tables, as the release contract requires.
const profileTemplate = readFileSync(new URL("../templates/cargo-profiles.toml", import.meta.url), "utf8");
export const defaultBuildInputs = { include: ["package.json", "pnpm-lock.yaml", "src-tauri/**"] };
export const hostPlatform = process.platform === "darwin" ? "mac" : process.platform === "win32" ? "win" : process.platform;

export function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

// RightKit QA is mandatory for every app: fixtures declare it at the latest published version unless a test opts out.
export function withQaAdoption(pkg, qa = true) {
  if (!qa) return pkg;
  const latest = versions.registry?.npm?.["@rightkit/qa"] ?? versions.stagedNpm["@rightkit/qa"];
  return { ...pkg, devDependencies: { ...pkg.devDependencies, "@rightkit/qa": latest } };
}

// A pnpm lock that resolves every exact @rightkit/* pin of the fixture package, with a deterministic integrity.
export function pnpmLockFor(pkg) {
  const pins = Object.entries({ ...pkg.dependencies, ...pkg.devDependencies }).filter(([name, spec]) => name.startsWith("@rightkit/") && /^\d+\.\d+\.\d+$/.test(spec));
  return `lockfileVersion: '9.0'\n${pins.length ? `\npackages:\n\n${pins.map(([name, version]) => `  '${name}@${version}':\n    resolution: {integrity: sha512-${Buffer.from(`${name}@${version}`).toString("hex")}==}\n`).join("\n")}` : ""}`;
}

export function fixture({ signed = true, publish = false, signingContract = "windows-raw-exe-authenticode-before-nsis-v1", prePackageFiles = ["raw.exe"], signFiles = ["fixture.exe"], macSigning = false, macNotarize = { file: "fixture.zip" }, packageJson, packageCommand, cargoManifest, cargoConfig, repoCargoConfig, siblingCargoManifest, buildInputs = defaultBuildInputs, platform = "win", hostedWorkflows, includeCargoSources = true, qa = true, configExtra = {}, targetExtra = {}, files = {} } = {}) {
  const fixtureRoot = tempRoot("right-release-test-");
  const dir = repoCargoConfig ? path.join(fixtureRoot, "apps", "fixture") : fixtureRoot;
  mkdirSync(dir, { recursive: true });
  const config = path.join(dir, "right-release.config.mjs");
  const fixturePackage = withQaAdoption(packageJson ?? {
    name: "fixture",
    scripts: { [`release:patch:${platform}`]: `right-release --platform ${platform} --tier patch` },
    packageManager: versions.packageManager,
    dependencies: {
      "@rightkit/license": versions.npm["@rightkit/license"],
      "@rightkit/logs": versions.npm["@rightkit/logs"],
    },
    devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] },
  }, qa);
  writeFileSync(path.join(dir, "package.json"), JSON.stringify(fixturePackage, null, 2));
  writeFileSync(
    config,
    `export default ${JSON.stringify({
      schema: 1,
      app: "fixture",
      version: "1.0.0",
      packageManager: "pnpm",
      checks: [],
      // What the seal and the upload read from the config (the admission's seal-routes and distribution-provider rows).
      distribution: { provider: "r2-rightapps", updaterPublicKey: "RWQfixtureUpdaterPublicKey" },
      ...(hostedWorkflows ? { hostedWorkflows } : {}),
      ...configExtra,
      targets: {
        [platform]: {
          ...(buildInputs ? { buildInputs } : {}),
          signed,
          package: packageCommand ?? { cmd: "node", args: ["-e", "process.exit(0)"] },
          ...(publish ? { publish: { cmd: "node", args: ["publish-update.mjs"] } } : {}),
          artifacts: [],
          ...(platform === "win" ? { signingContract, prePackage: { cmd: "node", args: ["-e", "process.exit(0)"] }, sign: { prePackageFiles, files: signFiles } } : {}),
          ...(platform === "mac" && macSigning ? { signingContract: "macos-developer-id-notarized-portable-v1", prePackage: { cmd: "node", args: ["-e", "process.exit(0)"] }, sign: { prePackageFiles: ["fixture"] }, notarize: macNotarize } : {}),
          updater: { artifacts: [{ file: "fixture.exe", signature: "fixture.exe.sig", platform: "windows-x86_64", key: "fixture/updates/windows/current/fixture.exe" }] },
          hardening: [],
          ...targetExtra,
        },
      },
    })};\n`,
  );
  writeFileSync(path.join(dir, "pnpm-lock.yaml"), pnpmLockFor(fixturePackage), "utf8");
  const tauriDir = path.join(dir, "src-tauri");
  mkdirSync(tauriDir, { recursive: true });
  if (includeCargoSources) {
    writeFileSync(path.join(tauriDir, "Cargo.toml"), `[package]\nname = "fixture-tauri"\nversion = "0.0.0"\nedition = "2021"\n[lib]\npath = "lib.rs"\n${profileTemplate}`, "utf8");
    writeFileSync(path.join(tauriDir, "Cargo.lock"), "# fixture lock\n", "utf8");
  }
  writeFileSync(path.join(tauriDir, "lib.rs"), "pub fn fixture() {}\n", "utf8");
  // A Windows NSIS target needs the NSIS template that upgrades in place (the admission's nsis-upgrade-contract row).
  if (platform === "win" && signingContract === "windows-raw-exe-authenticode-before-nsis-v1") {
    writeFileSync(path.join(tauriDir, "tauri.conf.json"), JSON.stringify({ bundle: { windows: { nsis: { template: "installer.nsi" } } } }), "utf8");
    writeFileSync(path.join(tauriDir, "installer.nsi"), "!define RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE\n", "utf8");
  }
  if (cargoManifest) {
    const crates = [...cargoManifest.matchAll(/^(rightkit-[a-z0-9-]+)\s*=\s*"=([^"]+)"/gm)];
    writeFileSync(path.join(dir, "Cargo.lock"), `version = 4\n${crates.map(([, name, version]) => `\n[[package]]\nname = "${name}"\nversion = "${version}"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "${createHash("sha256").update(`${name}@${version}`).digest("hex")}"\n`).join("")}`, "utf8");
    writeFileSync(path.join(dir, "Cargo.toml"), `${cargoManifest}\n[lib]\npath = "fixture.rs"\n${profileTemplate}`, "utf8");
    writeFileSync(path.join(dir, "fixture.rs"), "", "utf8");
  }
  if (cargoConfig) {
    const cargoDir = path.join(dir, ".cargo");
    mkdirSync(cargoDir, { recursive: true });
    writeFileSync(path.join(cargoDir, "config.toml"), cargoConfig, "utf8");
  }
  if (repoCargoConfig) {
    const cargoDir = path.join(fixtureRoot, ".cargo");
    mkdirSync(cargoDir, { recursive: true });
    writeFileSync(path.join(cargoDir, "config.toml"), repoCargoConfig, "utf8");
  }
  if (siblingCargoManifest) {
    const sibling = path.join(fixtureRoot, "docs", "archived-workspace");
    mkdirSync(sibling, { recursive: true });
    writeFileSync(path.join(sibling, "Cargo.toml"), siblingCargoManifest, "utf8");
  }
  for (const [relative, text] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(dir, relative)), { recursive: true });
    writeFileSync(path.join(dir, relative), text, "utf8");
  }
  git(fixtureRoot, "init", "--initial-branch", "main");
  git(fixtureRoot, "config", "user.email", "release-test@example.com");
  git(fixtureRoot, "config", "user.name", "Release Test");
  git(fixtureRoot, "add", ".");
  git(fixtureRoot, "commit", "-m", "initial");
  return config;
}

// node, pnpm, cargo and rustc as the host rows probe them. The shims print what a conforming host prints (pnpm at the
// packageManager pin of the feed), so a doctor or build under test sees the same tools on every machine; a case that
// needs another answer passes its own directory first on PATH. They live in the temp dir, so the broker guard leaves them alone.
// Every call is appended to the file RIGHT_RELEASE_TEST_TOOL_LOG names, so a test can prove which tool calls a refused build made.
let fakeToolsRoot;
let fakeCacheRoot;
// tempRoot() is removed when the test that created it ends; the directories every test shares live until the process exits.
function processRoot(prefix) {
  const root = mkdtempSync(path.join(os.tmpdir(), prefix));
  process.on("exit", () => rmSync(root, { recursive: true, force: true }));
  return root;
}
export function fakeTools({ pnpm = versions.packageManager.split("@")[1], rustc = "1.98.0", sccache = "0.15.0", pnpmRunExit = 0, shared = false } = {}) {
  const dir = shared ? processRoot("right-release-tools-") : tempRoot("right-release-tools-");
  const shim = (name, body) => {
    const log = `if (process.env.RIGHT_RELEASE_TEST_TOOL_LOG) require("node:fs").appendFileSync(process.env.RIGHT_RELEASE_TEST_TOOL_LOG, "${name} " + process.argv.slice(2).join(" ") + "\\n");`;
    writeFileSync(path.join(dir, `${name}.js`), `#!/usr/bin/env node\n${log}\n${body}\n`, { mode: 0o755 });
    if (process.platform === "win32") writeFileSync(path.join(dir, `${name}.cmd`), `@echo off\r\nnode "%~dp0${name}.js" %*\r\n`);
    else writeFileSync(path.join(dir, name), `#!/bin/sh\nexec node "$(dirname "$0")/${name}.js" "$@"\n`, { mode: 0o755 });
  };
  shim("pnpm", `if (process.argv[2] === "--version") console.log(${JSON.stringify(pnpm)}); else { console.log("pnpm " + process.argv.slice(2).join(" ")); process.exit(${Number(pnpmRunExit)}); }`);
  // `cargo metadata` is what the Cargo contract reads. With these shims first on PATH a bare `cargo` is no longer the command the broker
  // guard answers (on hosted CI it is not intercepted at all), so the shim hands the call to the guard under the client name it always
  // answers (rustright cargo ...) and relays the same metadata JSON the guard computes from the real manifest.
  // The call goes through background-process.mjs (the one module every release subprocess launches through), so the shim text
  // itself never names the child-process module and the hidden-launch policy test has nothing to flag.
  const spawnModule = JSON.stringify(new URL("../background-process.mjs", import.meta.url).href);
  shim("cargo", `if (process.argv[2] === "metadata") {
  import(${spawnModule}).then(({ spawnSync }) => {
    const answered = spawnSync("rustright", ["cargo", ...process.argv.slice(2)], { encoding: "utf8", cwd: process.cwd() });
    if (answered.error) { console.error("fixture cargo shim: the broker guard stub (test-support/broker-guard.mjs) is not loaded, so cargo metadata has no answer: " + answered.error.message); process.exit(1); }
    process.stdout.write(answered.stdout ?? "");
    process.stderr.write(answered.stderr ?? "");
    process.exit(answered.status ?? 1);
  }, (error) => { console.error("fixture cargo shim: cannot load background-process.mjs: " + error.message); process.exit(1); });
} else console.log("cargo ${rustc} (fixture)");`);
  shim("sccache", `console.log("sccache ${sccache}");`);
  // The signing probes of the host rows: a Mac with one Developer ID identity and a stored notary profile, and a Windows host with
  // the credential provider, signtool, the Azure dlib and its metadata (withFakeTools points the Azure variables at the files).
  shim("security", `if (process.argv[2] === "find-identity") console.log('  1) 0123456789ABCDEF0123456789ABCDEF01234567 "Developer ID Application: Fixture Inc (TEAM123456)"\\n     1 valid identities found');`);
  shim("xcrun", `console.log("{}");`);
  shim("rightkit-credentials", `console.log("rightkit-credentials 0.2.2");`);
  for (const file of ["signtool.exe", "Azure.CodeSigning.Dlib.dll", "azure-metadata.json"]) writeFileSync(path.join(dir, file), "{}\n");
  shim("rustc", `console.log(process.argv.includes("-vV") ? ${JSON.stringify(`rustc ${rustc} (fixture)\nbinary: rustc\nhost: x86_64-unknown-linux-gnu\nrelease: ${rustc}`)} : "rustc ${rustc} (fixture)");`);
  return dir;
}

/** `env` with the fake tools first on PATH (the key keeps its host casing: Windows spells it Path). */
export function withFakeTools(env, dir = (fakeToolsRoot ??= fakeTools({ shared: true }))) {
  const key = Object.keys(env).find((name) => name.toLowerCase() === "path") ?? "PATH";
  return {
    [key]: `${dir}${delimiter}${env[key] ?? ""}`,
    AZURE_SIGNTOOL_PATH: path.join(dir, "signtool.exe"),
    AZURE_CODESIGN_DLIB_PATH: path.join(dir, "Azure.CodeSigning.Dlib.dll"),
    AZURE_ARTIFACT_SIGNING_METADATA: path.join(dir, "azure-metadata.json"),
  };
}

export function managedEnv(config, { worker = false, platform = "win", registry = {}, env = {}, tools = true } = {}) {
  // The doctor's free-space floor is about the machine running the tests, not the code under test.
  // The registries are answered offline from the fixture's own locks; a worker run carries the admitted state the
  // controller (build-release.mjs) would have left for this exact release.
  // The host rows of the admission look for node, pnpm, cargo and rustc: the fake tools answer them the same on every host.
  // The registry observations a doctor or build caches go to a temp root, never to the user's cache.
  fakeCacheRoot ??= processRoot("right-release-cache-");
  return { ...process.env, ...(tools ? withFakeTools(process.env) : {}), RIGHT_RELEASE_MIN_FREE_GB: "1", RIGHT_RELEASE_CACHE_ROOT: fakeCacheRoot, ...(config ? registryEnv(path.dirname(config), registry) : {}), ...(config && worker ? admittedWorkerEnv(config, platform) : {}), ...env };
}

export const lastPlatform = (args, fallback) => (args.lastIndexOf("--platform") >= 0 ? args[args.lastIndexOf("--platform") + 1] : fallback);

// The doctor is release.mjs --doctor, which is what `right-release doctor` runs once the dispatcher's app check (cli/right-release.mjs)
// is green. The tests start it directly: they test the doctor's own rows, not the app check that guards the dispatcher.
export function runDoctorWith(config, registry, ...args) {
  return spawnSync(process.execPath, [release, "--doctor", "--config", config, "--platform", hostPlatform, ...args], {
    encoding: "utf8",
    env: managedEnv(config, { registry }),
  });
}

export function runDoctor(config, ...args) {
  return spawnSync(process.execPath, [release, "--doctor", "--config", config, "--platform", hostPlatform, ...args], {
    encoding: "utf8",
    env: managedEnv(config),
  });
}

export function editConfig(config, edit) {
  const text = readFileSync(config, "utf8");
  const prefix = "export default ";
  const value = JSON.parse(text.slice(prefix.length, text.lastIndexOf(";")));
  edit(value, config);
  writeFileSync(config, `${prefix}${JSON.stringify(value)};\n`);
  const dir = path.dirname(config);
  git(dir, "add", ".");
  git(dir, "commit", "-m", "edit config");
  return config;
}

