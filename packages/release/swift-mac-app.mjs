// Assemble a macOS .app bundle from a SwiftPM executable, then sign (and
// optionally notarize) it through the existing RightKit signers. Signing is
// inside-out: nested code first (no entitlements), then the bundle. This module
// owns bundle assembly only; sign-macos.mjs and notarize-macos.mjs own signing.

import { cpSync, existsSync, mkdirSync, openSync, readSync, closeSync, readdirSync, rmSync, statSync, writeFileSync, lstatSync, chmodSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";

const fail = (code, message) => Object.assign(new Error(message), { code });
const here = path.dirname(fileURLToPath(import.meta.url));
const xml = (v) => String(v).replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);

function plistValue(value, indent) {
  const pad = "\t".repeat(indent);
  if (typeof value === "string") return `${pad}<string>${xml(value)}</string>`;
  if (typeof value === "boolean") return `${pad}<${value}/>`;
  if (Number.isInteger(value)) return `${pad}<integer>${value}</integer>`;
  if (typeof value === "number") return `${pad}<real>${value}</real>`;
  if (Array.isArray(value)) return `${pad}<array>\n${value.map((v) => plistValue(v, indent + 1)).join("\n")}\n${pad}</array>`;
  if (value && typeof value === "object") {
    const rows = Object.keys(value).sort().map((k) => `${pad}\t<key>${xml(k)}</key>\n${plistValue(value[k], indent + 1)}`);
    return `${pad}<dict>\n${rows.join("\n")}\n${pad}</dict>`;
  }
  throw fail("MAC_APP_PLIST", `unsupported plist value: ${value}`);
}

export function generateInfoPlist(o) {
  const errors = [];
  if (!/^[A-Za-z0-9][A-Za-z0-9._ -]*$/.test(o.name ?? "")) errors.push("name must be a simple bundle name");
  if (!/^[A-Za-z0-9.-]+\.[A-Za-z0-9.-]+$/.test(o.bundleId ?? "")) errors.push("bundleId must be reverse-DNS");
  if (!/^\d+(\.\d+){0,2}$/.test(o.version ?? "")) errors.push("version must be 1-3 numeric parts");
  if (!/^\d+(\.\d+){0,2}$/.test(o.build ?? o.version ?? "")) errors.push("build must be 1-3 numeric parts");
  if (!o.executable || /[\\/]/.test(o.executable)) errors.push("executable must be a flat file name");
  if (errors.length) throw fail("MAC_APP_INVALID", errors.join("; "));
  const info = {
    CFBundleDevelopmentRegion: "en", CFBundleExecutable: o.executable, CFBundleIdentifier: o.bundleId,
    CFBundleInfoDictionaryVersion: "6.0", CFBundleName: o.name, CFBundleDisplayName: o.displayName ?? o.name,
    CFBundlePackageType: "APPL", CFBundleShortVersionString: o.version, CFBundleVersion: o.build ?? o.version,
    LSMinimumSystemVersion: o.minimumSystemVersion ?? "14.0", NSHighResolutionCapable: true, NSPrincipalClass: "NSApplication",
    ...(o.iconFile ? { CFBundleIconFile: path.basename(o.iconFile) } : {}),
    ...(o.infoPlistExtra ?? {}),
  };
  return `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0">\n${plistValue(info, 0)}\n</plist>\n`;
}

const MACHO = new Set([0xfeedface, 0xfeedfacf, 0xcefaedfe, 0xcffaedfe, 0xcafebabe, 0xbebafeca]);
function isMachO(file) {
  const fd = openSync(file, "r"); const b = Buffer.alloc(4);
  try { return readSync(fd, b, 0, 4, 0) === 4 && MACHO.has(b.readUInt32BE(0)); } finally { closeSync(fd); }
}
const BUNDLE_DIR = /\.(framework|app|xpc|bundle|appex)$/;

/** Nested code to sign before the app: Mach-O files and code bundles, deepest first. Main executable excluded. */
export function discoverNestedCode(app, mainName) {
  const main = path.join(app, "Contents", "MacOS");
  const found = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) {
        walk(full);
        if (/\.framework$/.test(entry.name) || (BUNDLE_DIR.test(entry.name) && existsSync(path.join(full, "Contents")))) found.push(full);
      } else if (entry.isFile() && full !== main && !full.startsWith(main + path.sep) && isMachO(full)) found.push(full);
    }
  };
  for (const sub of ["Contents/Frameworks", "Contents/Resources", "Contents/PlugIns", "Contents/XPCServices", "Contents/Helpers", "Contents/Library"]) {
    const dir = path.join(app, sub);
    if (existsSync(dir)) walk(dir);
  }
  const mainDir = path.join(app, "Contents", "MacOS");
  for (const entry of readdirSync(mainDir)) { /* helpers beside main: sign them too */
    const full = path.join(mainDir, entry);
    if (entry !== mainName && lstatSync(full).isFile() && isMachO(full)) found.push(full);
  }
  const depth = (p) => p.split(path.sep).length;
  return [...new Set(found)].sort((a, b) => depth(b) - depth(a) || a.localeCompare(b));
}

/**
 * Assemble `<out>/<name>.app`. options: { executable (SwiftPM binary), name, bundleId, version, build,
 * resources[] (files/dirs, e.g. SwiftPM *.bundle, copied into Contents/Resources), frameworks[] (dylibs/.frameworks
 * copied into Contents/Frameworks), iconFile, infoPlistExtra, out }.
 */
export function assembleMacApp(o, { run = spawnSync, platform = process.platform } = {}) {
  const exe = path.resolve(o.executable ?? "");
  if (!existsSync(exe) || !statSync(exe).isFile()) throw fail("MAC_APP_INVALID", `executable not found: ${exe}`);
  const execName = o.executableName ?? path.basename(exe);
  const app = path.join(path.resolve(o.out), `${o.name}.app`);
  const plist = generateInfoPlist({ ...o, executable: execName });
  rmSync(app, { recursive: true, force: true });
  const contents = path.join(app, "Contents");
  for (const dir of ["MacOS", "Resources", ...(o.frameworks?.length ? ["Frameworks"] : [])]) mkdirSync(path.join(contents, dir), { recursive: true });
  cpSync(exe, path.join(contents, "MacOS", execName)); chmodSync(path.join(contents, "MacOS", execName), 0o755);
  const copyInto = (list, sub) => {
    for (const item of list ?? []) {
      const source = path.resolve(item);
      if (!existsSync(source)) throw fail("MAC_APP_INVALID", `missing input: ${source}`);
      cpSync(source, path.join(contents, sub, path.basename(source)), { recursive: true, verbatimSymlinks: true });
    }
  };
  copyInto(o.resources, "Resources"); copyInto(o.frameworks, "Frameworks");
  if (o.iconFile) copyInto([o.iconFile], "Resources");
  writeFileSync(path.join(contents, "Info.plist"), plist);
  writeFileSync(path.join(contents, "PkgInfo"), "APPL????");
  if (o.frameworks?.length && platform === "darwin") {
    // SwiftPM binaries do not search Contents/Frameworks by default.
    const probe = run("otool", ["-l", path.join(contents, "MacOS", execName)], { encoding: "utf8" });
    if (!probe.error && !/@executable_path\/\.\.\/Frameworks/.test(probe.stdout ?? "")) {
      const r = run("install_name_tool", ["-add_rpath", "@executable_path/../Frameworks", path.join(contents, "MacOS", execName)], { encoding: "utf8" });
      if (r.error || r.status !== 0) throw fail("MAC_APP_RPATH", `install_name_tool failed: ${r.error?.message ?? r.stderr}`);
    }
  }
  return { app, nested: discoverNestedCode(app, execName), executable: execName };
}

/** Sign inside-out through sign-macos.mjs, then optionally notarize via notarize-macos.mjs. */
export function signAndNotarizeMacApp({ app, nested, entitlements, receiptDir, notarize = false, profile, dryRun = false },
  { run = spawnSync, platform = process.platform, log = (e) => console.error(JSON.stringify(e)) } = {}) {
  const plan = [];
  const node = (script, args) => { plan.push([script, ...args]); return args; };
  const receipts = path.resolve(receiptDir);
  const dry = dryRun ? ["--dry-run"] : [];
  if (!dryRun && platform !== "darwin") throw fail("MAC_APP_NATIVE_HOST_REQUIRED", "macOS signing and notarization must run on macOS");
  const calls = [];
  if (nested.length) calls.push(["sign-macos.mjs", [...dry, "--receipt", path.join(receipts, "app-nested-sign.receipt.json"), ...nested]]);
  calls.push(["sign-macos.mjs", [...dry, "--receipt", path.join(receipts, "app-sign.receipt.json"), ...(entitlements ? ["--entitlements", path.resolve(entitlements)] : []), app]]);
  let zip;
  if (notarize) {
    zip = `${app}.zip`;
    calls.push(["__ditto__", ["-c", "-k", "--keepParent", app, zip]]);
    calls.push(["notarize-macos.mjs", [...dry, "--receipt", path.join(receipts, "app-notarize.receipt.json"), ...(profile ? ["--profile", profile] : []), "--app", app, zip]]);
  }
  if (!dryRun) mkdirSync(receipts, { recursive: true });
  for (const [script, args] of calls) {
    node(script, args);
    if (dryRun && script === "__ditto__") continue;
    const r = script === "__ditto__" ? run("/usr/bin/ditto", args, { encoding: "utf8" })
      : run(process.execPath, [path.join(here, script), ...args], { encoding: "utf8" });
    if (r.error || r.status !== 0) throw fail("MAC_APP_COMMAND_FAILED", `${script} failed: ${r.error?.message ?? r.stderr ?? r.stdout ?? r.status}`);
    log({ event: "mac-app-step", script, ok: true });
  }
  return { plan, zip };
}
