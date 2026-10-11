#!/usr/bin/env node
// right-release swift-package <windows-stage|msix|mac-app> --config <swift-package.json> [options]
//
// One product config (schema 1) drives all three steps; paths resolve against the config file.
// Build-only: nothing here uploads or publishes.
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assembleMacApp, signAndNotarizeMacApp } from "./swift-mac-app.mjs";
import { buildMsix } from "./msix-build.mjs";
import { stageSwiftWindows } from "./swift-windows-stage.mjs";

const fail = (code, message) => Object.assign(new Error(message), { code });

export function loadSwiftPackageConfig(file) {
  const configPath = path.resolve(file ?? "");
  if (!existsSync(configPath)) throw fail("SWIFT_PACKAGE_CONFIG", `config not found: ${configPath}`);
  let config;
  try { config = JSON.parse(readFileSync(configPath, "utf8")); } catch (e) { throw fail("SWIFT_PACKAGE_CONFIG", `invalid JSON: ${e.message}`); }
  if (config.schema !== 1) throw fail("SWIFT_PACKAGE_CONFIG", "config.schema must be 1");
  if (!config.name || !config.version) throw fail("SWIFT_PACKAGE_CONFIG", "config.name and config.version are required");
  return { config, base: path.dirname(configPath) };
}

const rel = (base, value) => (value == null ? value : path.resolve(base, value));
const relList = (base, list) => (list ?? []).map((v) => rel(base, v));

export function windowsStageOptions({ config, base }) {
  const w = config.windows;
  if (!w?.exe || !w?.out) throw fail("SWIFT_PACKAGE_CONFIG", "windows.exe and windows.out are required");
  return { exe: rel(base, w.exe), out: rel(base, w.stageDir ?? `${w.out}.stage`), searchDirs: relList(base, w.searchDirs),
    include: relList(base, w.include), systemDlls: w.systemDlls, dependencies: w.dependencies, architecture: w.architecture, subsystem: w.subsystem };
}

export function msixOptions({ config, base }, staged) {
  const w = config.windows;
  const manifest = { name: w.packageName ?? config.name, version: w.msixVersion ?? `${config.version.replace(/^(\d+\.\d+\.\d+).*$/, "$1")}.0`,
    architecture: staged.architecture, publisher: w.publisher, publisherDisplayName: w.publisherDisplayName,
    displayName: config.displayName ?? config.name, description: config.description, executable: staged.exe,
    dependencies: w.dependencies, capabilities: w.capabilities, minWindowsVersion: w.minWindowsVersion };
  const assets = Object.fromEntries(Object.entries(w.assets ?? {}).map(([k, v]) => [k, rel(base, v)]));
  return { manifest, assets, out: rel(base, w.out), stageDir: staged.out };
}

export function macOptions({ config, base }) {
  const m = config.mac;
  if (!m?.executable || !m?.bundleId || !m?.out) throw fail("SWIFT_PACKAGE_CONFIG", "mac.executable, mac.bundleId and mac.out are required");
  return { executable: rel(base, m.executable), name: m.appName ?? config.displayName ?? config.name, displayName: config.displayName, bundleId: m.bundleId,
    version: config.version, build: m.build, minimumSystemVersion: m.minimumSystemVersion, resources: relList(base, m.resources),
    frameworks: relList(base, m.frameworks), iconFile: rel(base, m.iconFile), infoPlistExtra: m.infoPlist, out: rel(base, m.out) };
}

export async function main(argv, deps = {}) {
  const [command, ...rest] = argv;
  const flag = (name) => { const i = rest.indexOf(name); return i >= 0 ? rest[i + 1] : undefined; };
  const has = (name) => rest.includes(name);
  const log = deps.log ?? ((v) => console.log(JSON.stringify(v, null, 2)));
  if (!["windows-stage", "msix", "mac-app"].includes(command)) {
    throw fail("SWIFT_PACKAGE_USAGE", "usage: swift-package <windows-stage|msix|mac-app> --config <file> [--dry-run] [--no-sign] [--notarize]");
  }
  const loaded = loadSwiftPackageConfig(flag("--config"));
  if (command === "windows-stage") return log(stageSwiftWindows(windowsStageOptions(loaded)));
  if (command === "msix") {
    const staged = stageSwiftWindows(windowsStageOptions(loaded));
    const options = msixOptions(loaded, staged);
    return log(buildMsix({ ...options, sign: !has("--no-sign"), dryRun: has("--dry-run") }, deps.msix));
  }
  const options = macOptions(loaded);
  const assembled = assembleMacApp(options, deps.mac);
  const out = { app: assembled.app, nested: assembled.nested };
  if (has("--sign") || has("--notarize")) {
    out.signing = signAndNotarizeMacApp({ app: assembled.app, nested: assembled.nested,
      entitlements: rel(loaded.base, loaded.config.mac.entitlements), receiptDir: path.join(options.out, "receipts"),
      notarize: has("--notarize"), profile: flag("--profile"), dryRun: has("--dry-run") }, deps.mac);
  }
  return log(out);
}

const self = (file) => { try { return path.resolve(file) === fileURLToPath(import.meta.url); } catch { return false; } };
if (process.argv[1] && self(process.argv[1])) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(`right-release swift-package: [${error.code ?? "SWIFT_PACKAGE_FAILED"}] ${error.message}`);
    process.exitCode = error.code === "SWIFT_PACKAGE_USAGE" ? 2 : 1;
  });
}
