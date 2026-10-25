#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { compareVersions } from "./rightkit-sync.mjs";
import {
  DISK_EXIT_WARNING, applySweep, checkDisk, ensureScratchRoots, formatBytes, listOpenPaths, logEvent, planSweep, platformKey, purgeHoldings, readDiskPolicy, setWorkspaceSweepAllowed,
} from "./disk-policy.mjs";
import { evaluateBinding, installStableRoot, readScheduleBinding, selfCheck } from "./disk-stable-root.mjs";

export const LAUNCHD_LABEL = "dev.rightkit.disk-hygiene";
export const WINDOWS_TASK = "RightKit Disk Hygiene";

function renderFloor(f) {
  if (f.error) return `${f.label}: free space cannot be read (${f.error})  WARN`;
  return `${f.label}: ${formatBytes(f.freeBytes)} free of ${formatBytes(f.totalBytes)} (floor ${formatBytes(f.thresholdBytes)})${f.warn ? "  WARN" : ""}${f.hardWarn ? `  BELOW THE ${formatBytes(f.hardFreeBytes)} BUILD REFUSAL LEVEL` : ""}`;
}

export function renderCheck(result) {
  const lines = (result.floors ?? [result.floor]).map(renderFloor);
  for (const w of result.writers) {
    const size = w.error ? `error: ${w.error}` : w.matches === 0 ? "absent" : `${formatBytes(w.bytes)} / cap ${formatBytes(w.capBytes)}`;
    lines.push(`  ${w.id.padEnd(28)} ${w.action.padEnd(6)} ${size}${w.over ? "  OVER CAP" : ""}`);
  }
  for (const u of result.unmanaged ?? []) {
    lines.push(`  unmanaged remainder on ${u.floor}  ${formatBytes(u.unmanagedBytes)} (estimate: used ${formatBytes(u.usedBytes)} - writers ${formatBytes(u.writerBytes)})`);
    if (u.uncoveredTopLevel.length) {
      lines.push(`Uncovered top-level on ${u.floor} (${u.uncoveredTopLevel.length}):`);
      for (const e of u.uncoveredTopLevel) lines.push(`  ${(e.symlink ? "symlink" : formatBytes(e.bytes)).padStart(9)}  ${e.path}`);
    }
  }
  if (result.strays?.length) {
    lines.push(`Stray cargo targets (${result.strays.length}):`);
    for (const t of result.strays) lines.push(`  ${formatBytes(t.bytes).padStart(9)}  ${String(t.ageDays).padStart(4)}d  ${t.path}`);
  }
  if (result.unknown?.length) {
    lines.push(`Unknown top-level entries (${result.unknown.length}):`);
    for (const u of result.unknown) lines.push(`  ${(u.symlink ? "symlink" : formatBytes(u.bytes)).padStart(9)}  ${String(u.ageDays).padStart(4)}d  ${u.path}`);
  }
  for (const w of result.warnings) lines.push(`WARNING: ${w}`);
  return lines.join("\n");
}

export function renderSweep(plan, applied, { all = false } = {}) {
  const lines = [`${applied ? "Swept" : "Dry run: would move"} ${plan.candidates.length} item(s), ${formatBytes(plan.totalBytes)} (stale > ${plan.staleDays} days)`];
  const rows = applied ? applied.results : plan.candidates;
  const byWriter = new Map();
  for (const r of rows) byWriter.set(r.writer, [...(byWriter.get(r.writer) ?? []), r]);
  for (const [writer, items] of byWriter) {
    const total = items.reduce((s, r) => s + r.bytes, 0);
    lines.push(`  ${writer.padEnd(26)} ${String(items.length).padStart(4)} item(s) ${formatBytes(total).padStart(9)}`);
    for (const r of [...items].sort((a, b) => b.bytes - a.bytes).slice(0, all ? Infinity : 5)) lines.push(`      ${formatBytes(r.bytes).padStart(9)}  ${r.path}${r.destination ? ` -> ${r.destination}` : ""}${r.ok === false ? `  FAILED: ${r.error}` : ""}`);
    if (!all && items.length > 5) lines.push(`      ... ${items.length - 5} more`);
  }
  const counts = {};
  for (const s of plan.skipped) counts[s.reason] = (counts[s.reason] ?? 0) + 1;
  const reportOnly = plan.skipped.filter((s) => s.reason === "report-only");
  if (reportOnly.length) lines.push(`  workspace-unknown is report-only on this host: ${reportOnly.length} unknown entr${reportOnly.length === 1 ? "y" : "ies"} will not be moved (opt in with: right-release disk allow-workspace-sweep)`);
  if (plan.skipped.length) lines.push(`  skipped: ${Object.entries(counts).map(([k, v]) => `${v} ${k}`).join(", ")}`);
  return lines.join("\n");
}

function parse(rest) {
  const known = new Set(["--json", "--dry-run", "--delete", "--all", "--days", "--policy"]);
  const opts = { json: false, dryRun: false, deleteMode: false, all: false, days: undefined, policy: undefined };
  for (let i = 0; i < rest.length; i += 1) {
    const a = rest[i];
    if (!known.has(a)) throw new Error(`disk: unknown option ${a}`);
    if (a === "--json") opts.json = true;
    else if (a === "--dry-run") opts.dryRun = true;
    else if (a === "--delete") opts.deleteMode = true;
    else if (a === "--all") opts.all = true;
    else { const v = rest[++i]; if (!v) throw new Error(`disk: ${a} needs a value`); if (a === "--days") opts.days = Number(v); else opts.policy = v; }
  }
  if (opts.days !== undefined && !(opts.days >= 0)) throw new Error("disk: --days must be a non-negative number");
  if (opts.dryRun && opts.deleteMode) throw new Error("disk sweep accepts either --dry-run or --delete");
  return opts;
}

// A path inside any git checkout (a `.git` entry on an ancestor) is development code, never an installed package.
export function insideGitCheckout(file, exists = existsSync) {
  // A pnpm store is an install location even when it sits inside a workspace that is itself a git checkout (the Mac keeps
  // .pnpm-store under /Volumes/D/claude).
  if (file.split(/[\\/]+/).some((seg) => seg === ".pnpm-store" || seg === ".pnpm")) return false;
  let dir = path.dirname(file);
  for (;;) {
    if (exists(path.join(dir, ".git"))) return true;
    const up = path.dirname(dir);
    if (up === dir) return false;
    dir = up;
  }
}

const CLI_TAIL = path.join("@rightkit", "release", "cli", "right-release.mjs");
const SHIM_TARGET = /(%~dp0|%dp0%|\$basedir|\$PSScriptRoot)?([^\s"'%$=;~]*node_modules[\\/]@rightkit[\\/]release[\\/]cli[\\/]right-release\.mjs)/;

/** Target `cli/right-release.mjs` behind the `right-release` bin on PATH: the realpath of the bin when it is the script
 * itself, otherwise the path a pnpm/npm shim (.cmd, .ps1 or sh script) names, resolved against the shim's directory. */
export function cliFromPathBin({ env = process.env, exists = existsSync, readFile = (f) => readFileSync(f, "utf8") } = {}) {
  const found = [];
  for (const dir of String(env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean)) {
    for (const name of ["right-release", "right-release.cmd", "right-release.ps1", "right-release.mjs"]) {
      const bin = path.join(dir, name);
      if (!exists(bin)) continue;
      let real = bin;
      try { real = realpathSync(bin); } catch { /* keep lexical */ }
      if (real.endsWith(path.join("cli", "right-release.mjs"))) { found.push(real); continue; }
      try {
        const m = SHIM_TARGET.exec(readFile(real));
        if (m) found.push(path.resolve(dir, (m[1] ? m[2].replace(/^[\\/]+/, "") : m[2]).split(/[\\/]/).join(path.sep)));
      } catch { /* unreadable shim */ }
    }
  }
  return found;
}

const versionOf = (cli, readFile) => {
  try { return JSON.parse(readFile(path.join(path.dirname(cli), "..", "package.json"))).version; } catch { return "0.0.0"; }
};

/** Global-root candidates: `<root>/@rightkit/release/...` (npm, pnpm <= 10) and `<root>/<hash>/node_modules/@rightkit/release/...`
 * (pnpm 11 global layout), highest installed version first. */
export function cliFromGlobalRoots(roots, { exists = existsSync, readdir = (d) => readdirSync(d), readFile = (f) => readFileSync(f, "utf8") } = {}) {
  const found = [];
  for (const root of roots) {
    found.push(path.join(root, CLI_TAIL));
    let names = [];
    try { names = readdir(root); } catch { /* root absent */ }
    for (const name of names) found.push(path.join(root, name, "node_modules", CLI_TAIL));
  }
  return found.filter((f) => exists(f))
    .map((f) => ({ f, v: versionOf(f, readFile) }))
    .sort((x, y) => { try { return compareVersions(y.v, x.v); } catch { return 0; } })
    .map((x) => x.f);
}

/** Installed @rightkit/release CLI for the Windows task: explicit --cli / RIGHT_RELEASE_INSTALLED_CLI, else the target of the
 * `right-release` bin on PATH, else the global package roots (`npm root -g`, `pnpm root -g`, hashed pnpm 11 layout included).
 * Only a candidate inside a git checkout is refused; the installed package may itself run install. */
export function resolveInstalledCli({ explicit, env = process.env, globalRoots = defaultGlobalRoots, exists = existsSync, readFile, readdir, pathBin = cliFromPathBin } = {}) {
  const fixed = explicit || env.RIGHT_RELEASE_INSTALLED_CLI;
  const candidates = fixed
    ? [fixed]
    : [...pathBin({ env, exists, ...(readFile ? { readFile } : {}) }), ...cliFromGlobalRoots(globalRoots({ env }), { exists, ...(readFile ? { readFile } : {}), ...(readdir ? { readdir } : {}) })];
  for (const candidate of candidates) {
    if (!exists(candidate)) continue;
    let real = candidate;
    try { real = realpathSync(candidate); } catch { /* keep lexical */ }
    if (insideGitCheckout(real, exists)) continue;
    return candidate;
  }
  throw new Error("disk schedule install needs the installed @rightkit/release (not a checkout): install it globally or pass --cli <path to the installed cli/right-release.mjs>");
}

function defaultGlobalRoots() {
  const roots = [];
  for (const tool of ["npm", "pnpm"]) {
    const r = spawnSync(tool, ["root", "-g"], { encoding: "utf8", shell: process.platform === "win32" });
    const line = String(r.stdout ?? "").trim().split(/\r?\n/).pop();
    if (r.status === 0 && line) roots.push(line);
  }
  return roots;
}

export function scheduleSpec({ platform = process.platform, nodePath = process.execPath, cli = path.join(path.dirname(fileURLToPath(import.meta.url)), "cli", "right-release.mjs"), home = os.homedir(), hour = 3, minute = 30 } = {}) {
  if (platform === "darwin") {
    // launchd cannot open log files on an external volume for a background job (spawn fails with EX_CONFIG);
    // `disk run` writes its own structured log to disk-hygiene.jsonl on the heavy volume.
    const file = path.join(home, "Library", "LaunchAgents", `${LAUNCHD_LABEL}.plist`);
    const plist = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>${LAUNCHD_LABEL}</string>
<key>ProgramArguments</key><array><string>${nodePath}</string><string>${cli}</string><string>disk</string><string>run</string></array>
<key>StartCalendarInterval</key><dict><key>Hour</key><integer>${hour}</integer><key>Minute</key><integer>${minute}</integer></dict>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
<key>ProcessType</key><string>Background</string><key>LowPriorityIO</key><true/><key>Nice</key><integer>10</integer>
</dict></plist>
`;
    return { platform: "mac", file, plist };
  }
  const script = `$a = New-ScheduledTaskAction -Execute '${nodePath}' -Argument '"${cli}" disk run'
$t = New-ScheduledTaskTrigger -Daily -At ${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}
$s = New-ScheduledTaskSettingsSet -Hidden -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Hours 2)
$p = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\\$env:USERNAME" -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName '${WINDOWS_TASK}' -Action $a -Trigger $t -Settings $s -Principal $p -Force | Out-Null`;
  return { platform: "win", script };
}

const here = path.dirname(fileURLToPath(import.meta.url));

/** CLI to copy into the stable root: --cli / RIGHT_RELEASE_INSTALLED_CLI, else this very package when it is not a checkout
 * (the usual `right-release disk schedule install` from the installed package), else the discovered installed package. */
export function sourceCli({ explicit, env = process.env, self = path.join(here, "cli", "right-release.mjs"), resolve = resolveInstalledCli } = {}) {
  if (explicit || env.RIGHT_RELEASE_INSTALLED_CLI) return resolve({ explicit, env });
  let real = self;
  try { real = realpathSync(self); } catch { /* keep lexical */ }
  if (existsSync(self) && !insideGitCheckout(real)) return self;
  return resolve({ explicit, env });
}

function scheduleStatus({ platform, stdout, env = process.env, home = os.homedir() }) {
  let binding = null;
  let loaded = false;
  try { binding = readScheduleBinding({ platform, home }); } catch (error) { stdout(`status: cannot read the schedule (${error.message})`); return 1; }
  if (platform === "darwin") {
    const r = spawnSync("launchctl", ["print", `gui/${process.getuid()}/${LAUNCHD_LABEL}`], { encoding: "utf8" });
    loaded = r.status === 0;
  } else loaded = Boolean(binding);
  if (!binding) { stdout("not installed"); return 0; }
  const verdict = evaluateBinding(binding, { platform, env, home });
  stdout(`${loaded ? "installed" : "plist present but not loaded"} (${binding.source})`);
  stdout(`  target: ${binding.cli ?? "(none)"}`);
  if (verdict.ok) { stdout("  binding ok: stable root, target present"); return loaded ? 0 : 1; }
  for (const p of verdict.problems) stdout(`  PROBLEM: ${p}`);
  return 1;
}

function schedule(sub, { stdout, cli, platform = process.platform, env = process.env }) {
  if (!["install", "uninstall", "status"].includes(sub)) throw new Error("disk schedule install|uninstall|status");
  if (sub === "status") return scheduleStatus({ platform, stdout, env });
  const spec = scheduleSpec({ platform });
  if (platform === "darwin") {
    const uid = String(process.getuid());
    if (sub === "install") {
      const installed = installStableRoot({ cli: sourceCli({ explicit: cli, env }), platform, env });
      stdout(`${installed.unchanged ? "kept" : "installed"} @rightkit/release ${installed.version ?? "?"} in ${installed.stable}`);
      const bound = scheduleSpec({ platform, cli: installed.cli });
      mkdirSync(path.dirname(bound.file), { recursive: true });
      const policy = readDiskPolicy();
      mkdirSync(policy.heavyVolume[platformKey(platform)], { recursive: true });
      ensureScratchRoots({ policy, platform, env });
      writeFileSync(bound.file, bound.plist);
      spawnSync("launchctl", ["bootout", `gui/${uid}/${LAUNCHD_LABEL}`], { encoding: "utf8" });
      const r = spawnSync("launchctl", ["bootstrap", `gui/${uid}`, bound.file], { encoding: "utf8" });
      if (r.status !== 0) throw new Error(`launchctl bootstrap failed: ${r.stderr}`);
      stdout(`installed ${bound.file} (daily 03:30), bound to ${installed.cli}`);
    } else {
      spawnSync("launchctl", ["bootout", `gui/${uid}/${LAUNCHD_LABEL}`], { encoding: "utf8" });
      if (existsSync(spec.file)) unlinkSync(spec.file);
      stdout("uninstalled");
    }
    return 0;
  }
  if (sub === "install") {
    const installed = installStableRoot({ cli: sourceCli({ explicit: cli, env }), platform, env });
    stdout(`${installed.unchanged ? "kept" : "installed"} @rightkit/release ${installed.version ?? "?"} in ${installed.stable}`);
    ensureScratchRoots({ policy: readDiskPolicy(), platform, env });
    const bound = scheduleSpec({ platform, cli: installed.cli });
    const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", bound.script], { encoding: "utf8" });
    if (r.status !== 0) throw new Error(`Register-ScheduledTask failed: ${r.stderr || r.stdout}`);
    stdout(`installed scheduled task "${WINDOWS_TASK}" (daily 03:30, hidden), bound to ${installed.cli}`);
  } else {
    spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `Unregister-ScheduledTask -TaskName '${WINDOWS_TASK}' -Confirm:$false`], { encoding: "utf8" });
    stdout("uninstalled");
  }
  return 0;
}

// ---- agent-env and probe: keep every agent and child-tool write on the external root (Adrian, 2026-10-17) -------------------

const defaultAgentFs = { existsSync, statSync, mkdirSync, writeFileSync, unlinkSync, realpathSync };

const pathFor = (platform) => (platform === "win32" ? path.win32 : path.posix);

/** External root used when --root is absent: D:\ on Windows, /Volumes/D on macOS. No other platform is supported. */
export function defaultExternalRoot(platform = process.platform) {
  if (platform === "win32") return "D:\\";
  if (platform === "darwin") return "/Volumes/D";
  throw new Error(`disk agent-env: unsupported platform ${platform}; pass --root`);
}

function envValue(env, name, platform) {
  if (platform !== "win32") return env[name];
  const key = Object.keys(env).find((k) => k.toLowerCase() === name.toLowerCase());
  return key === undefined ? undefined : env[key];
}

/** The one table of agent-environment paths, relative to the external root, identical on Windows and macOS (these are the D:
 * paths RustRight already applied to the user environment). RustRight's broker imports this. `agentHome` rows are emitted only
 * with --include-agent-homes. CARGO_TARGET_DIR is deliberately absent: RustRight owns cargo targets. */
export const AGENT_ENV_TABLE = Object.freeze([
  { name: "TEMP", segments: ["agent-cache", "tmp"] },
  { name: "TMP", segments: ["agent-cache", "tmp"] },
  { name: "TMPDIR", segments: ["agent-cache", "tmp"] },
  { name: "CLAUDE_CODE_TMPDIR", segments: ["agent-cache", "tmp"] },
  { name: "XDG_CACHE_HOME", segments: ["agent-cache", "xdg"] },
  { name: "XDG_DATA_HOME", segments: ["agent-cache", "xdg-data"] },
  { name: "XDG_STATE_HOME", segments: ["agent-cache", "xdg-state"] },
  { name: "XDG_CONFIG_HOME", segments: ["agent-cache", "xdg-config"] },
  { name: "npm_config_cache", segments: [".dev-cache", "npm-cache"] },
  { name: "PNPM_STORE_DIR", segments: ["agent-cache", "pnpm-store"] },
  { name: "YARN_CACHE_FOLDER", segments: ["agent-cache", "yarn"] },
  { name: "PIP_CACHE_DIR", segments: [".dev-cache", "pip"] },
  { name: "UV_CACHE_DIR", segments: [".dev-cache", "uv"] },
  { name: "HF_HOME", segments: [".dev-cache", "huggingface"] },
  { name: "TORCH_HOME", segments: ["agent-cache", "torch"] },
  { name: "TRANSFORMERS_CACHE", segments: ["agent-cache", "transformers"] },
  { name: "PLAYWRIGHT_BROWSERS_PATH", segments: ["agent-cache", "playwright"] },
  { name: "PUPPETEER_CACHE_DIR", segments: ["agent-cache", "puppeteer"] },
  { name: "NUGET_PACKAGES", segments: ["agent-cache", "nuget"] },
  { name: "GOCACHE", segments: ["agent-cache", "go-build"] },
  { name: "GOMODCACHE", segments: ["agent-cache", "go-mod"] },
  { name: "DOTNET_CLI_HOME", segments: ["agent-cache", "dotnet"] },
  { name: "ELECTRON_CACHE", segments: ["agent-cache", "electron"] },
  { name: "DENO_DIR", segments: ["agent-cache", "deno"] },
  { name: "CODEX_HOME", segments: ["agent-home", "codex"], agentHome: true },
  { name: "CLAUDE_CONFIG_DIR", segments: ["agent-home", "claude"], agentHome: true },
]);

/** Ordered variable map for one external root from AGENT_ENV_TABLE. Pure. */
export function agentEnvMap({ root, platform = process.platform, includeAgentHomes = false } = {}) {
  if (!root) throw new Error("agentEnvMap needs a root");
  const p = pathFor(platform);
  return AGENT_ENV_TABLE.filter((e) => includeAgentHomes || !e.agentHome).map((e) => ({ name: e.name, value: p.join(root, ...e.segments) }));
}

export const AGENT_ENV_NOTES = [
  "CARGO_TARGET_DIR is intentionally NOT set: RustRight owns cargo targets.",
  "CODEX_HOME and CLAUDE_CONFIG_DIR are only emitted with --include-agent-homes (moving them requires restarting every chat).",
];

const shQuote = (v) => `'${String(v).replace(/'/g, `'\\''`)}'`;
const psQuote = (v) => `'${String(v).replace(/'/g, "''")}'`;

/** Render the map as a shell script, a PowerShell script, a cmd batch or JSON. Pure. */
export function renderAgentEnv(map, { format, root, notes = AGENT_ENV_NOTES } = {}) {
  if (format === "json") return JSON.stringify({ root, env: Object.fromEntries(map.map((m) => [m.name, m.value])), notes }, null, 2);
  if (format === "sh") return [...notes.map((n) => `# ${n}`), ...map.map((m) => `export ${m.name}=${shQuote(m.value)}`)].join("\n");
  if (format === "ps1") return [...notes.map((n) => `# ${n}`), ...map.map((m) => `$env:${m.name} = ${psQuote(m.value)}`)].join("\n");
  if (format === "cmd") return [...notes.map((n) => `rem ${n}`), ...map.map((m) => `set "${m.name}=${m.value}"`)].join("\r\n");
  throw new Error(`disk agent-env: unknown --format ${format} (sh|ps1|cmd|json)`);
}

function parseAgentEnv(rest, platform) {
  const opts = { root: undefined, format: platform === "win32" ? "ps1" : "sh", check: false, includeAgentHomes: false };
  for (let i = 0; i < rest.length; i += 1) {
    const a = rest[i];
    if (a === "--check") opts.check = true;
    else if (a === "--include-agent-homes") opts.includeAgentHomes = true;
    else if (a === "--root" || a === "--format") {
      const v = rest[++i];
      if (!v) throw new Error(`disk agent-env: ${a} needs a value`);
      if (a === "--root") opts.root = v; else opts.format = v;
    } else throw new Error(`disk agent-env: unknown option ${a}`);
  }
  if (!["sh", "ps1", "cmd", "json"].includes(opts.format)) throw new Error(`disk agent-env: unknown --format ${opts.format} (sh|ps1|cmd|json)`);
  return opts;
}

const LOCAL_UNC_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "[::1]", "."]);

/** Realpath of `value`, resolving its nearest existing ancestor when the path itself does not exist yet. Null when nothing resolves. */
function realpathOfExisting(value, realpath) {
  let current = value;
  let rest = "";
  for (;;) {
    try { return rest ? path.posix.join(realpath(current), rest) : realpath(current); } catch { /* try the parent */ }
    const up = path.posix.dirname(current);
    if (up === current) return null;
    rest = rest ? `${path.posix.basename(current)}/${rest}` : path.posix.basename(current);
    current = up;
  }
}

/** True when `value` lies on the internal drive, or cannot be proven external. Windows: a path on the system drive, including the
 * `\\?\C:\`, `\\.\C:\` and `\\?\UNC\localhost\C$\` / `\\localhost\C$\` spellings; any other path on this machine's own host name or
 * loopback, a relative path and any device or volume-GUID path are unprovable and count as internal. macOS: any path outside
 * /Volumes, and a /Volumes entry that resolves outside it (`/Volumes/Macintosh HD` is a symlink to /). Pure apart from the
 * injectable `realpath` used on macOS. */
export function pointsAtInternalDrive(value, { platform = process.platform, env = {}, realpath = realpathSync } = {}) {
  const v = String(value ?? "");
  if (platform === "win32") {
    const system = String(envValue(env, "SystemDrive", platform) ?? "C:").slice(0, 1).toUpperCase();
    let p = v.replace(/\//g, "\\");
    let unc = null;
    const extended = /^\\\\[?.]\\(.*)$/.exec(p);
    if (extended) {
      const tail = extended[1];
      if (/^UNC\\/i.test(tail)) unc = tail.slice(4);
      else if (/^[A-Za-z]:/.test(tail)) p = tail;
      else return true;
    } else if (/^\\\\[^\\]/.test(p)) unc = p.slice(2);
    if (unc !== null) {
      const [host = "", share = ""] = unc.split("\\");
      const hostName = host.toLowerCase();
      const own = String(envValue(env, "COMPUTERNAME", platform) ?? "").toLowerCase();
      if (!host) return true;
      if (!LOCAL_UNC_HOSTS.has(hostName) && !(own && hostName === own)) return false;
      const admin = /^([A-Za-z])\$$/.exec(share);
      return admin ? admin[1].toUpperCase() === system : true;
    }
    const m = /^([A-Za-z]):/.exec(p);
    if (!m) return true;
    return m[1].toUpperCase() === system;
  }
  if (!v.startsWith("/Volumes/")) return true;
  if (platform === "darwin") {
    const real = realpathOfExisting(v, realpath);
    if (real !== null && !real.startsWith("/Volumes/")) return true;
  }
  return false;
}

function isUnder(value, root, platform) {
  const p = pathFor(platform);
  const norm = (s) => { const n = p.resolve(s); return platform === "win32" ? n.toLowerCase() : n; };
  const r = norm(root);
  const v = norm(value);
  return v === r || v.startsWith(r.endsWith(p.sep) ? r : r + p.sep);
}

function uniqueProbeName(nonce) {
  return `.rk-disk-probe-${process.pid}-${nonce ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`}`;
}

/** Create then immediately remove one 0-byte uniquely named file in `dir`. Writes nothing else. Pure with injected fs. */
const BLOCKED_CODES = new Set(["EACCES", "EPERM", "EROFS"]);

/** Outcome: `allowed` (created and removed), `blocked` (EACCES/EPERM/EROFS), or `error` (missing directory or any other
 * failure, with its code). A failed removal is `allowed` with code `UNLINK_<code>` and `leftover` set. */
export function probeDirectory(dir, { fs = defaultAgentFs, nonce, platform = process.platform } = {}) {
  let file;
  try {
    if (!dir) return { outcome: "error", code: "ENODIR" };
    if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) return { outcome: "error", code: "ENOENT" };
    file = pathFor(platform).join(dir, uniqueProbeName(nonce));
    fs.writeFileSync(file, "", { flag: "wx" });
  } catch (error) {
    const code = error.code ?? "EUNKNOWN";
    return { outcome: BLOCKED_CODES.has(code) ? "blocked" : "error", code };
  }
  try {
    fs.unlinkSync(file);
  } catch (error) {
    return { outcome: "allowed", code: `UNLINK_${error.code ?? "EUNKNOWN"}`, leftover: true, file };
  }
  return { outcome: "allowed", code: null };
}

/** Locations that must be blocked for agents: user profile root, local temp (AppData\Local\Temp / default OS temp), OS temp, and
 * the external root. */
export function probeLocations({ platform = process.platform, env = process.env, home = os.homedir(), tmpdir = os.tmpdir(), root } = {}) {
  const p = pathFor(platform);
  const localTemp = platform === "win32"
    ? p.join(envValue(env, "LOCALAPPDATA", platform) ?? p.join(home, "AppData", "Local"), "Temp")
    : (envValue(env, "TMPDIR", platform) ?? tmpdir);
  return [
    { location: "profile", dir: home },
    { location: "local-temp", dir: localTemp },
    { location: "os-temp", dir: tmpdir },
    { location: "external-root", dir: root ?? defaultExternalRoot(platform) },
  ];
}

export const PROBE_SCHEMA = "rightkit.disk.probe.v1";

/** Probe all four locations; the report is the exact `rightkit.disk.probe.v1` shape. `internalWritable` is true when any
 * internal location (profile, local-temp, os-temp) accepted the probe file. */
export function runProbe({ platform = process.platform, env = process.env, home, tmpdir, root, fs = defaultAgentFs, nonce, host = os.hostname() } = {}) {
  const locations = probeLocations({ platform, env, ...(home ? { home } : {}), ...(tmpdir ? { tmpdir } : {}), root });
  const probed = locations.map((l) => ({ l, r: probeDirectory(l.dir, { fs, nonce, platform }) }));
  const external = locations.find((l) => l.location === "external-root").dir;
  // Fail closed. An internal location is sealed only when the write was refused (EACCES, EPERM, EROFS) or the directory does not
  // exist (ENOENT); ENOSPC, EBUSY and every other error prove nothing, so they count against the probe. The external root must
  // accept the probe file, or agent writes have nowhere to go.
  const problems = [];
  for (const { l, r } of probed) {
    if (l.location === "external-root") {
      if (r.outcome !== "allowed") problems.push(`${l.location} ${l.dir} is not writable (${r.code})`);
    } else if (r.outcome === "allowed") problems.push(`${l.location} ${l.dir} is writable`);
    else if (r.outcome === "error" && r.code !== "ENOENT") problems.push(`${l.location} ${l.dir} could not be proven blocked (${r.code})`);
  }
  return {
    problems,
    report: {
      schema: PROBE_SCHEMA, host, root: external,
      results: probed.map(({ l, r }) => ({ location: l.location, path: l.dir, outcome: r.outcome, code: r.code })),
      internalWritable: probed.some(({ l, r }) => l.location !== "external-root" && r.outcome === "allowed"),
    },
    leftovers: probed.filter(({ r }) => r.leftover).map(({ r }) => r.file),
  };
}

export function renderProbe(report) {
  return [...report.results.map((r) => `${r.outcome.toUpperCase().padEnd(8)} ${r.location.padEnd(14)} ${r.path}${r.code ? `  (${r.code})` : ""}`), report.internalWritable ? "INTERNAL DRIVE IS WRITABLE" : "internal drive is not writable"].join("\n");
}

/** Compare the live environment with the map. Each row: ok (under root), elsewhere (external, not under root), internal (set on
 * the internal drive), unset (tool default lands on the internal drive). */
export function checkAgentEnv(map, { env = process.env, platform = process.platform, root } = {}) {
  return map.map(({ name, value }) => {
    const current = envValue(env, name, platform);
    let status;
    if (current === undefined || current === "") status = "unset";
    else if (pointsAtInternalDrive(current, { platform, env })) status = "internal";
    else if (isUnder(current, root, platform)) status = "ok";
    else status = "elsewhere";
    return { name, expected: value, current: current ?? null, status };
  });
}

function agentEnvCommand(rest, { env, stdout, platform, deps }) {
  const stderr = deps.stderr ?? console.error;
  const fs = deps.fs ?? defaultAgentFs;
  const opts = parseAgentEnv(rest, platform);
  const root = opts.root ?? defaultExternalRoot(platform);
  const fail = (message) => { stderr(`disk agent-env: ${message}; refusing (no fallback to the internal drive)`); return { exitCode: 1 }; };
  if (pointsAtInternalDrive(root, { platform, env, realpath: (p) => fs.realpathSync(p) })) return fail(`root ${root} is on the internal drive`);
  if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) return fail(`root ${root} is missing`);
  if (platform === "darwin") {
    let real = root;
    try { real = fs.realpathSync(root); } catch { /* keep lexical */ }
    if (!real.startsWith("/Volumes/")) return fail(`root ${root} resolves to ${real}, not an external volume`);
  }
  const writable = probeDirectory(root, { fs, nonce: deps.nonce, platform });
  if (writable.outcome !== "allowed" || writable.leftover) return fail(`root ${root} is not writable (${writable.code})`);
  const map = agentEnvMap({ root, platform, includeAgentHomes: opts.includeAgentHomes });
  if (opts.check) {
    const rows = checkAgentEnv(map, { env, platform, root });
    const bad = rows.filter((r) => r.status === "internal" || r.status === "unset");
    if (opts.format === "json") stdout(JSON.stringify({ root, ok: bad.length === 0, variables: rows }, null, 2));
    else {
      for (const r of rows) stdout(`${r.status.toUpperCase().padEnd(9)} ${r.name.padEnd(26)} ${r.current ?? "(unset)"}${r.status === "ok" ? "" : `  -> ${r.expected}`}`);
      stdout(bad.length ? `${bad.length} variable(s) point at the internal drive or are unset` : "all variables point at the external root");
    }
    return { exitCode: bad.length ? 1 : 0, rows };
  }
  for (const m of map) fs.mkdirSync(m.value, { recursive: true });
  stdout(renderAgentEnv(map, { format: opts.format, root }));
  return { exitCode: 0, map };
}

function probeCommand(rest, { env, stdout, platform, deps }) {
  let json = false;
  let root;
  for (let i = 0; i < rest.length; i += 1) {
    if (rest[i] === "--json") json = true;
    else if (rest[i] === "--root") { root = rest[++i]; if (!root) throw new Error("disk probe: --root needs a value"); }
    else throw new Error(`disk probe: unknown option ${rest[i]}`);
  }
  const { report, leftovers, problems } = runProbe({ platform, env, root, ...(deps.probe ?? {}), ...(deps.fs ? { fs: deps.fs } : {}), nonce: deps.nonce });
  stdout(json ? JSON.stringify(report) : renderProbe(report));
  const stderr = deps.stderr ?? console.error;
  for (const problem of problems) stderr(`disk probe: ${problem}`);
  for (const file of leftovers) stderr(`disk probe: could not remove ${file}`);
  return { exitCode: report.internalWritable || leftovers.length || problems.length ? 1 : 0, report, problems };
}

export async function runDiskCommand(args = process.argv.slice(2), { env = process.env, stdout = console.log, platform = process.platform, deps = {} } = {}) {
  const [command, ...rest] = args;
  if (command === "agent-env") return agentEnvCommand(rest, { env, stdout, platform, deps });
  if (command === "probe") return probeCommand(rest, { env, stdout, platform, deps });
  if (command === "schedule") {
    const at = rest.indexOf("--cli");
    const exitCode = schedule(rest[0], { stdout, cli: at === -1 ? undefined : rest[at + 1], platform, env });
    return { exitCode };
  }
  if (command === "allow-workspace-sweep") {
    const off = rest.includes("--off");
    const file = setWorkspaceSweepAllowed(!off, { platform, env, ...(deps.flagHome ? { home: deps.flagHome } : {}) });
    stdout(off ? `workspace-unknown sweep revoked on this host (${file}.revoked)` : `workspace-unknown sweep allowed on this host (${file}); review \`disk sweep --dry-run --all\` first, revoke with --off`);
    return { exitCode: 0 };
  }
  const opts = parse(rest);
  const policy = readDiskPolicy(opts.policy);
  const key = platformKey(platform);
  const logFile = path.join(policy.heavyVolume[key], "disk-hygiene.jsonl");
  const out = (value, text) => stdout(opts.json ? JSON.stringify(value) : text);
  const check = () => checkDisk({ policy, platform, env, ...deps.check });
  const plan = () => planSweep({ policy, platform, env, staleDays: opts.days, openPaths: platform === "win32" ? null : (deps.openPaths ?? listOpenPaths({ platform })), ...deps.plan });

  if (command === "check") {
    const result = check();
    out(result, renderCheck(result));
    return { exitCode: result.warn ? DISK_EXIT_WARNING : 0, result };
  }
  if (command === "sweep") {
    if (!opts.dryRun) ensureScratchRoots({ policy, platform, env });
    const p = plan();
    if (opts.dryRun) { out(p, renderSweep(p, undefined, { all: opts.all })); return { exitCode: 0, plan: p }; }
    const applied = applySweep({ plan: p, policy, platform, deleteMode: opts.deleteMode, ...deps.apply });
    out({ ...p, applied }, renderSweep(p, applied, { all: opts.all }));
    return { exitCode: applied.results.some((r) => !r.ok) ? 1 : 0, plan: p, applied };
  }
  if (command === "purge") {
    const purged = purgeHoldings({ policy, platform, env, days: opts.days, dryRun: opts.dryRun, ...deps.purge });
    out(purged, `${opts.dryRun ? "Dry run: would purge" : "Purged"} ${purged.results.length} holding dir(s)${purged.results.map((r) => `\n  ${formatBytes(r.bytes).padStart(9)}  ${r.path}${r.ok ? "" : `  FAILED: ${r.error}`}`).join("")}`);
    return { exitCode: purged.results.some((r) => !r.ok) ? 1 : 0, purged };
  }
  if (command === "run") {
    // Self-check first: a broken binding (non-stable target, missing package or dependency) is logged and fails the run.
    const self = (deps.selfCheck ?? selfCheck)({ moduleDir: path.resolve(here, "."), platform, env, ...(deps.selfCheckOptions ?? {}) });
    if (!self.ok) {
      for (const problem of self.problems) stdout(`ERROR: disk run self-check: ${problem}`);
      logEvent(logFile, { event: "disk.run.selfcheck.fail", problems: self.problems, version: self.version });
      return { exitCode: 2, selfCheck: self };
    }
    ensureScratchRoots({ policy, platform, env });
    const before = check();
    stdout(renderCheck(before));
    const p = plan();
    stdout(renderSweep(p));
    const applied = applySweep({ plan: p, policy, platform, ...deps.apply });
    stdout(`moved ${formatBytes(applied.movedBytes)} (${applied.results.filter((r) => r.ok).length}/${applied.results.length})`);
    // Only the agent volume running below its soft floor is an emergency: the undo window is spent
    // (days 0, so what this pass just moved is purged too). Warnings from other floors or writers are not.
    const squeezed = (before.floors ?? [before.floor]).find((f) => f.agentVolume && f.warn && !f.absent);
    const emergency = Boolean(squeezed);
    const purged = purgeHoldings({ policy, platform, env, ...(emergency ? { days: 0 } : {}), ...deps.purge });
    stdout(`purged ${purged.results.filter((r) => r.ok).length} holding dir(s), ${formatBytes(purged.purgedBytes)}`);
    if (emergency) {
      stdout("EMERGENCY: the agent volume is below its soft floor, holding folders were purged without their undo window");
      logEvent(logFile, { event: "disk.run.emergency", floor: squeezed.id ?? squeezed.label, freeBytes: squeezed.freeBytes, purgedBytes: purged.purgedBytes });
    }
    const after = check();
    const warn = after.warn;
    logEvent(logFile, { event: warn ? "disk.floor.warn" : "disk.run.ok", warnings: after.warnings, freeBytes: after.floor.freeBytes, movedBytes: applied.movedBytes, kept: p.skipped.filter((s) => s.reason === "kept").length });
    if (warn) stdout(`WARNING: ${after.warnings.join("; ")}`);
    return { exitCode: !self.ok ? 2 : warn ? DISK_EXIT_WARNING : 0, before, after, applied, purged, selfCheck: self };
  }
  throw new Error("disk usage: check|sweep [--dry-run|--delete] [--all]|allow-workspace-sweep [--off] [--days N]|purge [--dry-run] [--days N]|run|schedule install|uninstall|status|agent-env [--root dir] [--format sh|ps1|cmd|json] [--check] [--include-agent-homes]|probe  [--json]");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runDiskCommand().then((r) => process.exit(r.exitCode)).catch((e) => { console.error(e.message); process.exit(2); });
}
