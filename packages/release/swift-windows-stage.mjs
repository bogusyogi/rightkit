// Stage a SwiftPM-built Windows app for packaging: copy the executable and every
// app-local DLL it needs (transitively) into one flat directory, and fail closed
// on any dependency that is neither staged, a Windows system DLL, nor provided by
// a declared package dependency. Runs on any host (pure file + PE parsing).

import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { readPeImports } from "./pe-imports.mjs";

const fail = (code, message) => Object.assign(new Error(message), { code });

// Windows-owned DLLs present on every supported install. Anything else must ship.
export const WINDOWS_SYSTEM_DLLS = Object.freeze([
  "kernel32.dll", "kernelbase.dll", "ntdll.dll", "user32.dll", "gdi32.dll", "gdi32full.dll", "advapi32.dll", "shell32.dll",
  "shlwapi.dll", "ole32.dll", "oleaut32.dll", "combase.dll", "rpcrt4.dll", "sechost.dll", "ws2_32.dll", "wsock32.dll",
  "mswsock.dll", "iphlpapi.dll", "bcrypt.dll", "bcryptprimitives.dll", "ncrypt.dll", "crypt32.dll", "secur32.dll", "wintrust.dll",
  "userenv.dll", "version.dll", "winmm.dll", "comctl32.dll", "comdlg32.dll", "imm32.dll", "uxtheme.dll", "dwmapi.dll",
  "d2d1.dll", "d3d11.dll", "d3d12.dll", "dxgi.dll", "dwrite.dll", "dcomp.dll", "uiautomationcore.dll", "propsys.dll",
  "msctf.dll", "setupapi.dll", "cfgmgr32.dll", "shcore.dll", "powrprof.dll", "psapi.dll", "dbghelp.dll", "wldap32.dll",
  "normaliz.dll", "netapi32.dll", "winhttp.dll", "wininet.dll", "ucrtbase.dll", "msvcrt.dll", "oleacc.dll", "avrt.dll",
  "mf.dll", "mfplat.dll", "dnsapi.dll", "wtsapi32.dll", "winspool.drv", "ntmarta.dll", "dxcore.dll", "windows.ui.dll", "cabinet.dll",
]);

const isApiSet = (name) => /^(api|ext)-ms-win-[a-z0-9-]+-l\d+-\d+-\d+\.dll$/.test(name);
const lower = (value) => String(value).toLowerCase();
const isPe = (file) => /\.(exe|dll)$/i.test(file);
const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

function findInDirs(name, dirs) {
  for (const dir of dirs) {
    if (!existsSync(dir)) continue;
    const match = readdirSync(dir).find((entry) => lower(entry) === lower(name));
    if (match) return path.join(dir, match);
  }
  return null;
}

/**
 * options: { exe, out, searchDirs[], include[] (extra files copied verbatim),
 *   systemDlls[] (extra allowed), dependencies[{ name, provides[] }], architecture }
 * Returns a manifest; throws SWIFT_STAGE_UNRESOLVED listing every missing DLL.
 */
const PE_SUBSYSTEMS = { gui: 2, console: 3 };

export function stageSwiftWindows({ exe, out, searchDirs = [], include = [], systemDlls = [], dependencies = [], architecture, subsystem, clean = true } = {}) {
  if (!exe || !out) throw fail("SWIFT_STAGE_OPTIONS", "exe and out are required");
  exe = path.resolve(exe); out = path.resolve(out);
  if (!/\.exe$/i.test(exe) || !existsSync(exe) || !statSync(exe).isFile()) throw fail("SWIFT_STAGE_OPTIONS", `exe must be an existing .exe: ${exe}`);
  const dirs = [path.dirname(exe), ...searchDirs.map((dir) => path.resolve(dir))];
  const system = new Set([...WINDOWS_SYSTEM_DLLS, ...systemDlls].map(lower));
  const provided = new Map();
  for (const dep of dependencies) for (const dll of dep.provides ?? []) provided.set(lower(dll), dep.name);
  if (clean) rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });

  const staged = new Map(); // lower name -> { file(source), name }
  const queue = [{ name: path.basename(exe), source: exe, from: "<root>" }];
  const missing = new Map(); // lower name -> importers
  const seenSystem = new Set(), seenProvided = new Map();
  let arch = architecture;
  const extra = include.map((file) => path.resolve(file));
  for (const file of extra) {
    if (!existsSync(file) || !statSync(file).isFile()) throw fail("SWIFT_STAGE_OPTIONS", `include file missing: ${file}`);
    queue.push({ name: path.basename(file), source: file, from: "<include>" });
  }
  while (queue.length) {
    const item = queue.shift();
    const key = lower(item.name);
    if (staged.has(key)) continue;
    staged.set(key, item);
    if (!isPe(item.name)) continue;
    const info = readPeImports(readFileSync(item.source));
    if (!["x64", "arm64"].includes(info.architecture)) throw fail("SWIFT_STAGE_ARCH", `${item.source} is ${info.architecture}; only x64 and arm64 are supported`);
    arch ??= info.architecture;
    if (item.from === "<root>" && subsystem) {
      const want = PE_SUBSYSTEMS[subsystem];
      if (!want) throw fail("SWIFT_STAGE_OPTIONS", `windows.subsystem must be "gui" or "console", got ${JSON.stringify(subsystem)}`);
      if (info.subsystem !== want) {
        throw fail("SWIFT_STAGE_SUBSYSTEM", `${item.source} has PE subsystem ${info.subsystem}, expected ${want} (${subsystem})` +
          (subsystem === "gui" ? "; link with -Xlinker /SUBSYSTEM:WINDOWS -Xlinker /ENTRY:mainCRTStartup so launching it opens no console window" : ""));
      }
    }
    if (info.architecture !== arch) throw fail("SWIFT_STAGE_ARCH", `${item.source} is ${info.architecture} but package architecture is ${arch}`);
    for (const dep of [...info.imports, ...info.delayImports]) {
      const dk = lower(dep);
      if (staged.has(dk)) continue;
      const found = findInDirs(dep, dirs);
      if (found) { queue.push({ name: path.basename(found), source: found, from: item.name }); continue; }
      if (isApiSet(dk) || system.has(dk)) { seenSystem.add(dk); continue; }
      if (provided.has(dk)) { seenProvided.set(dk, provided.get(dk)); continue; }
      if (!missing.has(dk)) missing.set(dk, new Set());
      missing.get(dk).add(item.name);
    }
  }
  if (missing.size) {
    const detail = [...missing].map(([dll, by]) => `${dll} (needed by ${[...by].join(", ")})`).join("; ");
    throw fail("SWIFT_STAGE_UNRESOLVED", `unresolved DLL dependencies: ${detail}. Add the directory to searchDirs, stage it via include, or declare a package dependency that provides it`);
  }
  const files = [];
  for (const [, item] of [...staged].sort(([a], [b]) => a.localeCompare(b))) {
    const target = path.join(out, item.name);
    copyFileSync(item.source, target);
    files.push({ name: item.name, source: item.source, sha256: sha256(target), sizeBytes: statSync(target).size, requiredBy: item.from, pe: isPe(item.name) });
  }
  const manifest = {
    schema: 1, architecture: arch, exe: path.basename(exe), out, files,
    systemDlls: [...seenSystem].sort(),
    providedByDependencies: Object.fromEntries([...seenProvided].sort()),
  };
  writeFileSync(`${out}.stage.json`, `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}
