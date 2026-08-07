#!/usr/bin/env node
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";

const GB = 1024 ** 3;

export function heavyWorkRoot({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  if (env.RIGHTSUITE_HEAVY_WORK_ROOT) return path.resolve(env.RIGHTSUITE_HEAVY_WORK_ROOT);
  if (platform === "win32" || platform === "win") {
    if (!env.LOCALAPPDATA) throw new Error("LOCALAPPDATA is required for the RightSuite heavy-work guard");
    return path.win32.resolve(env.LOCALAPPDATA, "RightSuite", "heavy-work");
  }
  if (platform === "darwin" || platform === "mac") return path.join(home, "Library", "Caches", "RightSuite", "heavy-work");
  return path.resolve(env.XDG_CACHE_HOME || path.join(home, ".cache"), "rightsuite", "heavy-work");
}

export function heavyCommandEnvironment(env = process.env) {
  return {
    ...env,
    CARGO_BUILD_JOBS: env.CARGO_BUILD_JOBS || env.BUILD_GUARD_CARGO_JOBS || "2",
    RUST_TEST_THREADS: env.RUST_TEST_THREADS || env.BUILD_GUARD_TEST_THREADS || "2",
    RIGHTSUITE_HEAVY_WORK_OWNER: "1",
  };
}

export function parseMacResourceSnapshot({ memory = "", swap = "", thermal = "" } = {}) {
  const freePercent = Number(memory.match(/memory free percentage:\s*(\d+(?:\.\d+)?)%/i)?.[1]);
  const swapMatch = swap.match(/total\s*=\s*([\d.]+)M\s+used\s*=\s*([\d.]+)M/i);
  const speedLimit = Number(thermal.match(/CPU_Speed_Limit\s*=\s*(\d+)/i)?.[1]);
  const warning = Number(thermal.match(/(?:thermal|performance) warning level\s*[:=]\s*(\d+)/i)?.[1]);
  return {
    freePercent: Number.isFinite(freePercent) ? freePercent : null,
    swapTotalBytes: swapMatch ? Number(swapMatch[1]) * 1024 ** 2 : null,
    swapUsedBytes: swapMatch ? Number(swapMatch[2]) * 1024 ** 2 : null,
    thermalLimited: Number.isFinite(speedLimit) ? speedLimit < 100 : Number.isFinite(warning) ? warning > 0 : false,
  };
}

export function parseWindowsResourceSnapshot({ system = "", fallbackTotalBytes = os.totalmem(), fallbackFreeBytes = os.freemem() } = {}) {
  let parsed = {};
  try { parsed = JSON.parse(String(system).trim().replace(/^\uFEFF/, "")); } catch { /* use Node memory fallback */ }
  const totalMemoryBytes = Number(parsed.TotalBytes) > 0 ? Number(parsed.TotalBytes) : Number(fallbackTotalBytes);
  const freeMemoryBytes = Number(parsed.FreeBytes) >= 0 ? Number(parsed.FreeBytes) : Number(fallbackFreeBytes);
  const cpuLoadPercent = parsed.CpuLoadPercent === null || parsed.CpuLoadPercent === undefined ? null : Number(parsed.CpuLoadPercent);
  return {
    freePercent: totalMemoryBytes > 0 ? Math.round((freeMemoryBytes / totalMemoryBytes) * 1000) / 10 : null,
    totalMemoryBytes,
    freeMemoryBytes,
    cpuLoadPercent: Number.isFinite(cpuLoadPercent) ? cpuLoadPercent : null,
    thermalLimited: false,
  };
}

export function systemResourceSnapshot({ platform = process.platform, run = spawnSync, totalmem = os.totalmem, freemem = os.freemem } = {}) {
  if (platform === "win32" || platform === "win") {
    const script = "$os=Get-CimInstance Win32_OperatingSystem;$cpu=(Get-CimInstance Win32_Processor|Measure-Object LoadPercentage -Average).Average;[pscustomobject]@{TotalBytes=[double]$os.TotalVisibleMemorySize*1024;FreeBytes=[double]$os.FreePhysicalMemory*1024;CpuLoadPercent=[double]$cpu}|ConvertTo-Json -Compress";
    const result = run("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true });
    return { platform, ...parseWindowsResourceSnapshot({ system: result.status === 0 ? result.stdout : "", fallbackTotalBytes: totalmem(), fallbackFreeBytes: freemem() }) };
  }
  if (platform !== "darwin" && platform !== "mac") return { platform, freePercent: null, thermalLimited: false };
  const output = (command, args) => {
    const result = run(command, args, { encoding: "utf8", windowsHide: true });
    return result.status === 0 ? String(result.stdout ?? result.stderr ?? "") : "";
  };
  return {
    platform,
    ...parseMacResourceSnapshot({
      memory: output("memory_pressure", ["-Q"]),
      swap: output("sysctl", ["vm.swapusage"]),
      thermal: output("pmset", ["-g", "therm"]),
    }),
  };
}

export function resourceBlockers(snapshot, { minFreePercent = 15, maxCpuLoadPercent = 90 } = {}) {
  const blockers = [];
  if (snapshot.freePercent !== null && snapshot.freePercent < minFreePercent) {
    blockers.push(`memory free ${snapshot.freePercent}% < ${minFreePercent}%`);
  }
  if (Number.isFinite(snapshot.cpuLoadPercent) && snapshot.cpuLoadPercent > maxCpuLoadPercent) {
    blockers.push(`CPU load ${snapshot.cpuLoadPercent}% > ${maxCpuLoadPercent}%`);
  }
  if (snapshot.thermalLimited) blockers.push("macOS reports thermal/performance limiting");
  return blockers;
}

export function formatResourceSnapshot(snapshot) {
  const fields = [];
  if (snapshot.freePercent !== null) fields.push(`memory-free=${snapshot.freePercent}%`);
  if (Number.isFinite(snapshot.swapUsedBytes) && Number.isFinite(snapshot.swapTotalBytes)) {
    fields.push(`swap=${(snapshot.swapUsedBytes / GB).toFixed(1)}/${(snapshot.swapTotalBytes / GB).toFixed(1)}GiB`);
  }
  if (Number.isFinite(snapshot.cpuLoadPercent)) fields.push(`cpu-load=${snapshot.cpuLoadPercent}%`);
  if (snapshot.platform === "darwin" || snapshot.platform === "mac" || snapshot.thermalLimited) {
    fields.push(`thermal=${snapshot.thermalLimited ? "limited" : "ok"}`);
  }
  return fields.join(" ");
}

function processAlive(pid) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return false;
  try { process.kill(Number(pid), 0); return true; } catch { return false; }
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function readOwner(lockDir) {
  try { return JSON.parse(readFileSync(path.join(lockDir, "owner.json"), "utf8")); } catch { return null; }
}

export function acquireHeavyWorkSlot({
  root = heavyWorkRoot(), pid = process.pid, argv = process.argv, waitMs = Number(process.env.BUILD_GUARD_WAIT_SECS ?? 900) * 1000,
  pollMs = 1000, alive = processAlive, pause = sleep, snapshot = systemResourceSnapshot,
  minFreePercent = Number(process.env.BUILD_GUARD_MIN_MEMORY_FREE_PERCENT ?? 15), log = (message) => console.error(`[heavy-work] ${message}`),
  maxCpuLoadPercent = Number(process.env.BUILD_GUARD_MAX_CPU_LOAD_PERCENT ?? 90),
} = {}) {
  mkdirSync(root, { recursive: true });
  const lockDir = path.join(root, "slot");
  const token = randomUUID();
  const started = Date.now();
  let lastReport = 0;
  for (;;) {
    try {
      mkdirSync(lockDir);
      const fd = openSync(path.join(lockDir, "owner.json"), "wx");
      writeFileSync(fd, `${JSON.stringify({ schema: 1, pid: Number(pid), token, argv: [...argv], createdAt: new Date().toISOString() })}\n`);
      closeSync(fd);
      break;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      const owner = readOwner(lockDir);
      const incompleteAge = (() => { try { return Date.now() - statSync(lockDir).mtimeMs; } catch { return 0; } })();
      if (owner ? !alive(owner.pid) : incompleteAge > 10_000) {
        rmSync(lockDir, { recursive: true, force: true });
        continue;
      }
      if (Date.now() - started >= waitMs) throw new Error(`heavy-work slot timed out after ${waitMs}ms; holder pid ${owner?.pid ?? "starting"}`);
      if (Date.now() - lastReport >= 10_000) { log(`waiting for pid ${owner?.pid ?? "starting"}`); lastReport = Date.now(); }
      pause(Math.min(pollMs, Math.max(1, waitMs - (Date.now() - started))));
    }
  }

  for (;;) {
    const current = snapshot();
    const blockers = resourceBlockers(current, { minFreePercent, maxCpuLoadPercent });
    if (blockers.length === 0) {
      log(`admitted ${formatResourceSnapshot(current)} jobs=${heavyCommandEnvironment().CARGO_BUILD_JOBS}`);
      break;
    }
    if (Date.now() - started >= waitMs) {
      releaseOwned(lockDir, token);
      throw new Error(`resource admission timed out: ${blockers.join(", ")}`);
    }
    if (Date.now() - lastReport >= 10_000) { log(`resource wait: ${blockers.join(", ")}`); lastReport = Date.now(); }
    pause(Math.min(5000, Math.max(1, waitMs - (Date.now() - started))));
  }
  return { root, release: () => releaseOwned(lockDir, token) };
}

function releaseOwned(lockDir, token) {
  const owner = readOwner(lockDir);
  if (owner?.token === token) rmSync(lockDir, { recursive: true, force: true });
}

export async function runHeavyCommand(args, { env = process.env } = {}) {
  let command;
  let commandArgs;
  if (args[0] === "--shell-base64") {
    const source = Buffer.from(args[1] || "", "base64").toString("utf8");
    const shell = process.platform === "win32" ? (env.SHELL || "bash") : (env.SHELL || "/bin/zsh");
    command = shell; commandArgs = ["-lc", source];
  } else {
    const separator = args.indexOf("--");
    const values = separator >= 0 ? args.slice(separator + 1) : args;
    [command, ...commandArgs] = values;
  }
  if (!command) throw new Error("heavy-command: expected -- <command> or --shell-base64 <value>");
  if (env.RIGHTSUITE_HEAVY_WORK_OWNER === "1") return spawnAttached(command, commandArgs, env);
  const slot = acquireHeavyWorkSlot();
  try { return await spawnAttached(command, commandArgs, heavyCommandEnvironment(env)); } finally { slot.release(); }
}

function spawnAttached(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: process.cwd(), env, stdio: "inherit", windowsHide: true });
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve(code ?? (signal ? 1 : 0)));
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runHeavyCommand(process.argv.slice(2)).then((code) => { process.exitCode = code; }).catch((error) => { console.error(`heavy-command: ${error.message}`); process.exitCode = 1; });
}
