#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, unlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { compareVersions } from "./rightkit-sync.mjs";
import {
  DISK_EXIT_WARNING, applySweep, checkDisk, formatBytes, listOpenPaths, logEvent, planSweep, platformKey, purgeHoldings, readDiskPolicy,
} from "./disk-policy.mjs";

export const LAUNCHD_LABEL = "dev.rightkit.disk-hygiene";
export const WINDOWS_TASK = "RightKit Disk Hygiene";

export function renderCheck(result) {
  const lines = [`${result.floor.label}: ${formatBytes(result.floor.freeBytes)} free of ${formatBytes(result.floor.totalBytes)} (floor ${formatBytes(result.floor.thresholdBytes)})${result.floor.warn ? "  WARN" : ""}`];
  for (const w of result.writers) {
    const size = w.error ? `error: ${w.error}` : w.matches === 0 ? "absent" : `${formatBytes(w.bytes)} / cap ${formatBytes(w.capBytes)}`;
    lines.push(`  ${w.id.padEnd(28)} ${w.action.padEnd(6)} ${size}${w.over ? "  OVER CAP" : ""}`);
  }
  for (const w of result.warnings) lines.push(`WARNING: ${w}`);
  return lines.join("\n");
}

export function renderSweep(plan, applied) {
  const lines = [`${applied ? "Swept" : "Dry run: would move"} ${plan.candidates.length} item(s), ${formatBytes(plan.totalBytes)} (stale > ${plan.staleDays} days)`];
  const rows = applied ? applied.results : plan.candidates;
  const byWriter = new Map();
  for (const r of rows) byWriter.set(r.writer, [...(byWriter.get(r.writer) ?? []), r]);
  for (const [writer, items] of byWriter) {
    const total = items.reduce((s, r) => s + r.bytes, 0);
    lines.push(`  ${writer.padEnd(26)} ${String(items.length).padStart(4)} item(s) ${formatBytes(total).padStart(9)}`);
    for (const r of [...items].sort((a, b) => b.bytes - a.bytes).slice(0, 5)) lines.push(`      ${formatBytes(r.bytes).padStart(9)}  ${r.path}${r.destination ? ` -> ${r.destination}` : ""}${r.ok === false ? `  FAILED: ${r.error}` : ""}`);
    if (items.length > 5) lines.push(`      ... ${items.length - 5} more`);
  }
  const counts = {};
  for (const s of plan.skipped) counts[s.reason] = (counts[s.reason] ?? 0) + 1;
  if (plan.skipped.length) lines.push(`  skipped: ${Object.entries(counts).map(([k, v]) => `${v} ${k}`).join(", ")}`);
  return lines.join("\n");
}

function parse(rest) {
  const known = new Set(["--json", "--dry-run", "--delete", "--days", "--policy"]);
  const opts = { json: false, dryRun: false, deleteMode: false, days: undefined, policy: undefined };
  for (let i = 0; i < rest.length; i += 1) {
    const a = rest[i];
    if (!known.has(a)) throw new Error(`disk: unknown option ${a}`);
    if (a === "--json") opts.json = true;
    else if (a === "--dry-run") opts.dryRun = true;
    else if (a === "--delete") opts.deleteMode = true;
    else { const v = rest[++i]; if (!v) throw new Error(`disk: ${a} needs a value`); if (a === "--days") opts.days = Number(v); else opts.policy = v; }
  }
  if (opts.days !== undefined && !(opts.days >= 0)) throw new Error("disk: --days must be a non-negative number");
  if (opts.dryRun && opts.deleteMode) throw new Error("disk sweep accepts either --dry-run or --delete");
  return opts;
}

// A path inside any git checkout (a `.git` entry on an ancestor) is development code, never an installed package.
export function insideGitCheckout(file, exists = existsSync) {
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
    const logDir = "/Volumes/D/claude/tmp/swept";
    const file = path.join(home, "Library", "LaunchAgents", `${LAUNCHD_LABEL}.plist`);
    const plist = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>${LAUNCHD_LABEL}</string>
<key>ProgramArguments</key><array><string>${nodePath}</string><string>${cli}</string><string>disk</string><string>run</string></array>
<key>StartCalendarInterval</key><dict><key>Hour</key><integer>${hour}</integer><key>Minute</key><integer>${minute}</integer></dict>
<key>StandardOutPath</key><string>${logDir}/launchd.out.log</string>
<key>StandardErrorPath</key><string>${logDir}/launchd.err.log</string>
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

function schedule(sub, { stdout, cli }) {
  const platform = process.platform;
  const spec = scheduleSpec(platform === "darwin" || sub !== "install" ? { platform } : { platform, cli: resolveInstalledCli({ explicit: cli }) });
  if (platform === "darwin") {
    const uid = String(process.getuid());
    if (sub === "install") {
      mkdirSync(path.dirname(spec.file), { recursive: true });
      mkdirSync("/Volumes/D/claude/tmp/swept", { recursive: true });
      writeFileSync(spec.file, spec.plist);
      spawnSync("launchctl", ["bootout", `gui/${uid}/${LAUNCHD_LABEL}`], { encoding: "utf8" });
      const r = spawnSync("launchctl", ["bootstrap", `gui/${uid}`, spec.file], { encoding: "utf8" });
      if (r.status !== 0) throw new Error(`launchctl bootstrap failed: ${r.stderr}`);
      stdout(`installed ${spec.file} (daily 03:30)`);
    } else if (sub === "uninstall") {
      spawnSync("launchctl", ["bootout", `gui/${uid}/${LAUNCHD_LABEL}`], { encoding: "utf8" });
      if (existsSync(spec.file)) unlinkSync(spec.file);
      stdout("uninstalled");
    } else if (sub === "status") {
      const r = spawnSync("launchctl", ["print", `gui/${uid}/${LAUNCHD_LABEL}`], { encoding: "utf8" });
      stdout(r.status === 0 ? `installed (${spec.file})` : "not installed");
    } else throw new Error("disk schedule install|uninstall|status");
    return;
  }
  if (sub === "install") {
    const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", spec.script], { encoding: "utf8" });
    if (r.status !== 0) throw new Error(`Register-ScheduledTask failed: ${r.stderr || r.stdout}`);
    stdout(`installed scheduled task "${WINDOWS_TASK}" (daily 03:30, hidden)`);
  } else if (sub === "uninstall") {
    spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `Unregister-ScheduledTask -TaskName '${WINDOWS_TASK}' -Confirm:$false`], { encoding: "utf8" });
    stdout("uninstalled");
  } else if (sub === "status") {
    const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `Get-ScheduledTask -TaskName '${WINDOWS_TASK}' | Select-Object -ExpandProperty State`], { encoding: "utf8" });
    stdout(r.status === 0 ? `installed (${String(r.stdout).trim()})` : "not installed");
  } else throw new Error("disk schedule install|uninstall|status");
}

export async function runDiskCommand(args = process.argv.slice(2), { env = process.env, stdout = console.log, platform = process.platform, deps = {} } = {}) {
  const [command, ...rest] = args;
  if (command === "schedule") {
    const at = rest.indexOf("--cli");
    schedule(rest[0], { stdout, cli: at === -1 ? undefined : rest[at + 1] });
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
    const p = plan();
    if (opts.dryRun) { out(p, renderSweep(p)); return { exitCode: 0, plan: p }; }
    const applied = applySweep({ plan: p, policy, platform, deleteMode: opts.deleteMode, ...deps.apply });
    out({ ...p, applied }, renderSweep(p, applied));
    return { exitCode: applied.results.some((r) => !r.ok) ? 1 : 0, plan: p, applied };
  }
  if (command === "purge") {
    const purged = purgeHoldings({ policy, platform, env, days: opts.days, dryRun: opts.dryRun, ...deps.purge });
    out(purged, `${opts.dryRun ? "Dry run: would purge" : "Purged"} ${purged.results.length} holding dir(s)${purged.results.map((r) => `\n  ${formatBytes(r.bytes).padStart(9)}  ${r.path}${r.ok ? "" : `  FAILED: ${r.error}`}`).join("")}`);
    return { exitCode: purged.results.some((r) => !r.ok) ? 1 : 0, purged };
  }
  if (command === "run") {
    const before = check();
    stdout(renderCheck(before));
    const p = plan();
    stdout(renderSweep(p));
    const applied = applySweep({ plan: p, policy, platform, ...deps.apply });
    stdout(`moved ${formatBytes(applied.movedBytes)} (${applied.results.filter((r) => r.ok).length}/${applied.results.length})`);
    const purged = purgeHoldings({ policy, platform, env, ...deps.purge });
    stdout(`purged ${purged.results.filter((r) => r.ok).length} holding dir(s), ${formatBytes(purged.purgedBytes)}`);
    const after = check();
    const warn = after.warn;
    logEvent(logFile, { event: warn ? "disk.floor.warn" : "disk.run.ok", warnings: after.warnings, freeBytes: after.floor.freeBytes, movedBytes: applied.movedBytes });
    if (warn) stdout(`WARNING: ${after.warnings.join("; ")}`);
    return { exitCode: warn ? DISK_EXIT_WARNING : 0, before, after, applied, purged };
  }
  throw new Error("disk usage: check|sweep [--dry-run|--delete] [--days N]|purge [--dry-run] [--days N]|run|schedule install|uninstall|status  [--json]");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runDiskCommand().then((r) => process.exit(r.exitCode)).catch((e) => { console.error(e.message); process.exit(2); });
}
