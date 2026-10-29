#!/usr/bin/env node
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, statSync, writeFileSync, realpathSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "./background-process.mjs";
import { spawnUnderOwnership } from "./build-ownership.mjs";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";

import { resolveStableRoot } from "./cache-policy.mjs";

const GB = 1024 ** 3;
const SELF = fileURLToPath(import.meta.url);

export function heavyWorkRoot({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  return resolveStableRoot({
    platform,
    env,
    home,
    explicitOverride: env.RIGHTSUITE_HEAVY_WORK_ROOT,
    overrideName: "RIGHTSUITE_HEAVY_WORK_ROOT",
    darwinSegments: ["Library", "Caches", "RightSuite", "heavy-work"],
    win32EnvVar: "LOCALAPPDATA",
    win32Segments: ["RightSuite", "heavy-work"],
    otherSegments: ["rightsuite", "heavy-work"],
  });
}

export function heavyCommandEnvironment(env = process.env) {
  return {
    ...env,
    CARGO_BUILD_JOBS: env.CARGO_BUILD_JOBS || env.BUILD_GUARD_CARGO_JOBS || "2",
    RUST_TEST_THREADS: env.RUST_TEST_THREADS || env.BUILD_GUARD_TEST_THREADS || "2",
    RIGHTSUITE_HEAVY_WORK_OWNER: "1",
  };
}

export function heavyChildDetached({ slot, platform = process.platform } = {}) {
  return Boolean(slot) && platform !== "win32" && platform !== "win";
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
  try { process.kill(Number(pid), 0); return true; } catch (error) { return error?.code === "EPERM"; }
}

function processGroupAlive(pid) {
  try { process.kill(-Number(pid), 0); return true; } catch (error) { return error?.code === "EPERM"; }
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function readOwner(lockDir) {
  try {
    const owner = JSON.parse(readFileSync(path.join(lockDir, "owner.json"), "utf8"));
    try { return { ...owner, ...JSON.parse(readFileSync(path.join(lockDir, "child.json"), "utf8")) }; } catch { return owner; }
  } catch { return null; }
}

function writeTrackedChild(lockDir, owner, child) {
  const file = path.join(lockDir, "child.json");
  const temporary = path.join(lockDir, `.child-${owner.token}.tmp`);
  writeFileSync(temporary, `${JSON.stringify(child)}\n`, { mode: 0o600 });
  renameSync(temporary, file);
}

function matchingProcessIdentity(pid, expectedStartedAtMs, { alive = processAlive, startedAt = processStartedAt, assumeCurrentWhenStartUnavailable = false } = {}) {
  if (!alive(pid)) return false;
  if (!Number.isFinite(Number(expectedStartedAtMs))) return assumeCurrentWhenStartUnavailable;
  const observedStartedAtMs = startedAt(pid);
  if (!Number.isFinite(observedStartedAtMs)) return assumeCurrentWhenStartUnavailable;
  return Math.abs(observedStartedAtMs - Number(expectedStartedAtMs)) <= 2000;
}

export function sweepTrackedHeavyWorkOrphan({ root = heavyWorkRoot(), alive = processAlive, startedAt = processStartedAt, terminate = terminateProcessTree, log = (message) => console.error(`[heavy-work] ${message}`) } = {}) {
  const lockDir = path.join(root, "slot");
  const owner = readOwner(lockDir);
  if (!owner) return false;
  if (matchingProcessIdentity(owner.pid, owner.ownerStartedAtMs, { alive, startedAt, assumeCurrentWhenStartUnavailable: true })) return false;
  if (owner.childPid && alive(owner.childPid)) {
    if (!matchingProcessIdentity(owner.childPid, owner.childStartedAtMs, { alive, startedAt })) {
      throw new Error(`dead heavy-work owner pid ${owner.pid}; tracked child pid ${owner.childPid} identity changed; refusing to kill or admit`);
    }
    log(`reaping owned process tree pid ${owner.childPid} after stale owner pid ${owner.pid}`);
    terminate(owner.childPid);
  }
  rmSync(lockDir, { recursive: true, force: true });
  return true;
}

export function processStartedAt(pid, { platform = process.platform, run = spawnSync } = {}) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return null;
  const result = platform === "win32" || platform === "win"
    ? run("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", `(Get-Process -Id ${Number(pid)} -ErrorAction Stop).StartTime.ToUniversalTime().ToString('o')`], { encoding: "utf8", windowsHide: true })
    : run("ps", ["-p", String(pid), "-o", "lstart="], { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) return null;
  const value = Date.parse(String(result.stdout ?? "").trim());
  return Number.isFinite(value) ? value : null;
}

export function terminateProcessTree(pid, { platform = process.platform, run = spawnSync, kill = process.kill, treeAlive = processGroupAlive, pause = sleep } = {}) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return;
  if (platform === "win32" || platform === "win") {
    run("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
    return;
  }
  try { kill(-Number(pid), "SIGTERM"); } catch { try { kill(Number(pid), "SIGTERM"); } catch { return; } }
  pause(500);
  if (treeAlive(pid)) { try { kill(-Number(pid), "SIGKILL"); } catch { try { kill(Number(pid), "SIGKILL"); } catch { /* gone */ } } }
}

export async function watchOwnedProcessTree(
  { root, token, ownerPid, childPid, childStartedAtMs },
  { alive = processAlive, startedAt = processStartedAt, terminate = terminateProcessTree, pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms)) } = {},
) {
  const lockDir = path.join(root, "slot");
  while (alive(ownerPid)) {
    if (readOwner(lockDir)?.token !== token) return false;
    await pause(500);
  }
  const owner = readOwner(lockDir);
  if (owner?.token !== token || Number(owner.childPid) !== Number(childPid)) return false;
  if (alive(childPid)) {
    const observedStart = startedAt(childPid);
    if (!Number.isFinite(observedStart) || Math.abs(observedStart - Number(childStartedAtMs)) > 2000) {
      throw new Error(`watcher found reused child pid ${childPid}; refusing termination`);
    }
    terminate(childPid);
  }
  releaseOwned(lockDir, token);
  return true;
}

function spawnOwnerWatcher(details) {
  const payload = Buffer.from(JSON.stringify(details)).toString("base64url");
  const watcher = spawnUnderOwnership(spawn, process.execPath, [SELF, "--watch-owner-base64", payload], {
    detached: true, env: process.env, stdio: "ignore", windowsHide: true,
  });
  watcher.once("error", () => {
    const observedStart = processStartedAt(details.childPid);
    if (Number.isFinite(observedStart) && Math.abs(observedStart - Number(details.childStartedAtMs)) <= 2000) {
      terminateProcessTree(details.childPid);
    }
  });
  watcher.unref();
}

export function summarizePreBrokerWaits({ heavySlot, suiteSlot, heavyOwnerInherited = false } = {}) {
  const heavy = heavySlot?.waits ?? null;
  const suite = suiteSlot?.waits ?? null;
  const layers = [
    heavy ? { layer: "heavy-work-slot", ms: (heavy.slotWaitMs ?? 0) + (heavy.resourceWaitMs ?? 0), slotWaitMs: heavy.slotWaitMs ?? 0, resourceWaitMs: heavy.resourceWaitMs ?? 0, blockers: heavy.slotBlockers ?? [], resourceBlockers: heavy.resourceBlockers ?? [] }
      : { layer: "heavy-work-slot", ms: 0, skipped: heavyOwnerInherited ? "slot owned by parent heavy-command" : "not acquired" },
    suite ? { layer: "suite-build-slot", ms: suite.waitMs ?? 0, blockers: suite.blockers ?? [] } : { layer: "suite-build-slot", ms: 0, skipped: "inactive (non-shared cache mode)" },
  ];
  const totalMs = layers.reduce((sum, layer) => sum + layer.ms, 0);
  const summary = `${layers.map((layer) => `${layer.layer}=${layer.ms}ms${layer.skipped ? ` (${layer.skipped})` : ""}${layer.blockers?.length ? ` blocked-by ${layer.blockers.map((b) => typeof b === "string" ? b : `pid ${b.pid ?? "starting"}`).join(",")}` : ""}`).join("; ")}; total=${totalMs}ms`;
  return { layers, totalMs, summary, brokerQueue: "see broker receipt waits.brokerQueue (queueMs + blockers)" };
}

export function acquireHeavyWorkSlot({
  root = heavyWorkRoot(), pid = process.pid, argv = process.argv, waitMs = Number(process.env.BUILD_GUARD_WAIT_SECS ?? 900) * 1000,
  pollMs = 1000, alive = processAlive, pause = sleep, snapshot = systemResourceSnapshot,
  minFreePercent = Number(process.env.BUILD_GUARD_MIN_MEMORY_FREE_PERCENT ?? 15), log = (message) => console.error(`[heavy-work] ${message}`),
  maxCpuLoadPercent = Number(process.env.BUILD_GUARD_MAX_CPU_LOAD_PERCENT ?? 90),
  processStartedAt: startedAt = processStartedAt, terminate = terminateProcessTree, startWatcher = spawnOwnerWatcher,
} = {}) {
  mkdirSync(root, { recursive: true });
  const lockDir = path.join(root, "slot");
  const token = randomUUID();
  const started = Date.now();
  let lastReport = 0;
  // Wait attribution: who held the slot, and how long each layer made us wait.
  const waits = { layer: "heavy-work-slot", slotWaitMs: 0, slotBlockers: [], resourceWaitMs: 0, resourceBlockers: [], totalMs: 0 };
  const noteSlotBlocker = (owner) => {
    const identity = owner ? { pid: owner.pid ?? null, argv: Array.isArray(owner.argv) ? owner.argv.slice(0, 8).join(" ").slice(0, 200) : null, since: owner.createdAt ?? null } : { pid: null, argv: null, since: null, starting: true };
    if (!waits.slotBlockers.some((entry) => entry.pid === identity.pid && entry.since === identity.since)) waits.slotBlockers.push(identity);
  };
  for (;;) {
    try {
      mkdirSync(lockDir);
      const fd = openSync(path.join(lockDir, "owner.json"), "wx");
      writeFileSync(fd, `${JSON.stringify({ schema: 1, pid: Number(pid), ownerStartedAtMs: startedAt(pid), token, argv: [...argv], createdAt: new Date().toISOString() })}\n`);
      closeSync(fd);
      waits.slotWaitMs = Date.now() - started;
      break;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      const owner = readOwner(lockDir);
      noteSlotBlocker(owner);
      const incompleteAge = (() => { try { return Date.now() - statSync(lockDir).mtimeMs; } catch { return 0; } })();
      if (owner ? sweepTrackedHeavyWorkOrphan({ root, alive, startedAt, terminate, log }) : incompleteAge > 10_000) {
        if (!owner) rmSync(lockDir, { recursive: true, force: true });
        continue;
      }
      if (Date.now() - started >= waitMs) throw new Error(`heavy-work slot timed out after ${waitMs}ms; holder pid ${owner?.pid ?? "starting"}`);
      if (Date.now() - lastReport >= 10_000) { log(`waiting for pid ${owner?.pid ?? "starting"}`); lastReport = Date.now(); }
      pause(Math.min(pollMs, Math.max(1, waitMs - (Date.now() - started))));
    }
  }

  const resourceStarted = Date.now();
  for (;;) {
    const current = snapshot();
    const blockers = resourceBlockers(current, { minFreePercent, maxCpuLoadPercent });
    for (const blocker of blockers) if (!waits.resourceBlockers.includes(blocker)) waits.resourceBlockers.push(blocker);
    if (blockers.length === 0) {
      waits.resourceWaitMs = Date.now() - resourceStarted; waits.totalMs = Date.now() - started;
      log(`admitted ${formatResourceSnapshot(current)} jobs=${heavyCommandEnvironment().CARGO_BUILD_JOBS} waited slot=${waits.slotWaitMs}ms resource=${waits.resourceWaitMs}ms${waits.slotBlockers.length ? ` blockers=${waits.slotBlockers.map((entry) => `pid ${entry.pid ?? "starting"}`).join(",")}` : ""}`);
      break;
    }
    if (Date.now() - started >= waitMs) {
      releaseOwned(lockDir, token);
      throw new Error(`resource admission timed out: ${blockers.join(", ")}`);
    }
    if (Date.now() - lastReport >= 10_000) { log(`resource wait: ${blockers.join(", ")}`); lastReport = Date.now(); }
    pause(Math.min(5000, Math.max(1, waitMs - (Date.now() - started))));
  }
  return {
    root,
    waits,
    trackChild(childPid, childCommand = "") {
      const owner = readOwner(lockDir);
      if (owner?.token !== token) throw new Error("heavy-work slot ownership changed before child tracking");
      const childStartedAtMs = startedAt(childPid);
      if (!Number.isFinite(childStartedAtMs)) {
        if (!alive(childPid)) return false;
        throw new Error(`heavy-work could not verify child pid ${childPid} start time`);
      }
      writeTrackedChild(lockDir, owner, { childPid: Number(childPid), childCommand: String(childCommand), childStartedAtMs });
      startWatcher({ root, token, ownerPid: Number(pid), childPid: Number(childPid), childStartedAtMs });
      return true;
    },
    release: () => releaseOwned(lockDir, token),
  };
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
  try { return await spawnAttached(command, commandArgs, heavyCommandEnvironment(env), slot); } finally { slot.release(); }
}

function spawnAttached(command, args, env, slot) {
  return new Promise((resolve, reject) => {
    const child = spawnUnderOwnership(spawn, command, args, { cwd: process.cwd(), env, stdio: "inherit", windowsHide: true, detached: heavyChildDetached({ slot }) });
    try {
      if (child.pid && slot) slot.trackChild(child.pid, command);
    } catch (error) {
      child.once("error", () => {});
      if (child.pid) terminateProcessTree(child.pid);
      reject(error);
      return;
    }
    const handlers = new Map(["SIGINT", "SIGTERM", "SIGHUP"].map((signal) => [signal, () => {
      if (child.pid) terminateProcessTree(child.pid);
      process.exitCode = signal === "SIGINT" ? 130 : 143;
    }]));
    for (const [signal, handler] of handlers) process.once(signal, handler);
    const cleanup = () => { for (const [signal, handler] of handlers) process.removeListener(signal, handler); };
    child.once("error", (error) => { cleanup(); reject(error); });
    child.once("exit", (code, signal) => { cleanup(); resolve(code ?? (signal ? 1 : 0)); });
  });
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const args = process.argv.slice(2);
  const task = args[0] === "--watch-owner-base64"
    ? watchOwnedProcessTree(JSON.parse(Buffer.from(args[1] || "", "base64url").toString("utf8"))).then(() => 0)
    : runHeavyCommand(args);
  task.then((code) => { process.exitCode = code; }).catch((error) => { console.error(`heavy-command: ${error.message}`); process.exitCode = 1; });
}
