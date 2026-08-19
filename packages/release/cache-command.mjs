#!/usr/bin/env node
import { existsSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import {
  applyCachePrune,
  inspectCache,
  migrateLegacyCache,
  planCachePrune,
  readCachePolicy,
  resolveCacheLayout,
  resolveSharedCacheIdentity,
  resolveSharedCacheRoot,
} from "./cache-policy.mjs";
import { resolveReleaseLayout } from "./release-state.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";

// On a broker-managed host, CARGO_TARGET_DIR is not Cache V2's to own: the build
// broker unconditionally overrides it to its own per-namespace workspace, so the
// Cache V2 target layout under cacheRoot is legitimately empty (marker files only)
// while the real ~18 GB of build output lives in the broker's workspace. Inspecting,
// migrating, or pruning against that layout on a broker host reads real output as
// missing/corrupt and can recommend destroying it — never phrase this as a problem.
const BROKER_MANAGED_MESSAGE = "broker-managed host: the build broker owns CARGO_TARGET_DIR; Cache V2 target checks skipped";
const BROKER_GATED_COMMANDS = new Set(["status", "prune", "migrate"]);

export async function runCacheCommand(args = process.argv.slice(2), { env = process.env, stdout = console.log, stderr = console.error } = {}) {
  const [command, ...rest] = args;
  const json = rest.includes("--json");
  const platform = platformFrom(env);
  const cacheRoot = resolveSharedCacheRoot({ platform, env });
  if (BROKER_GATED_COMMANDS.has(command) && brokerManagedHost(env)) {
    const result = { schema: 1, command, cacheRoot, brokerManaged: true, skipped: true, message: BROKER_MANAGED_MESSAGE };
    if (json) stdout(JSON.stringify(result));
    else stdout(BROKER_MANAGED_MESSAGE);
    return result;
  }
  if (command === "status") {
    rejectUnknown(rest, new Set(["--json"]));
    const snapshot = inspectCache({ cacheRoot });
    return output({ schema: 1, command, ...snapshot, policy: readCachePolicy(env) }, json, stdout);
  }
  if (command === "prune") {
    rejectUnknown(rest, new Set(["--json", "--dry-run", "--apply"]));
    if (rest.includes("--dry-run") && rest.includes("--apply")) throw new Error("cache prune accepts either --dry-run or --apply");
    const dryRun = !rest.includes("--apply");
    const snapshot = inspectCache({ cacheRoot });
    const plan = planCachePrune({ snapshot, policy: readCachePolicy(env), protectedEntryIds: new Set(snapshot.entries.filter((entry) => entry.leased).map((entry) => entry.id)) });
    const result = dryRun ? { candidateIds: plan.candidateIds, removedIds: [], reclaimedBytes: 0, dryRun: true } : applyCachePrune({ plan, cacheRoot, dryRun: false });
    return output({ schema: 1, command, cacheRoot, dryRun, plan, ...result }, json, stdout);
  }
  if (command === "migrate") {
    const configPath = option(rest, "--config");
    if (!configPath) throw new Error("cache migrate requires --config <right-release.config.mjs>");
    rejectUnknown(rest.filter((value) => value !== "--config" && value !== configPath), new Set(["--json", "--dry-run", "--apply"]));
    if (rest.includes("--dry-run") && rest.includes("--apply")) throw new Error("cache migrate accepts either --dry-run or --apply");
    const configFile = path.resolve(configPath);
    const config = (await import(pathToFileURL(configFile).href)).default;
    if (!config?.app) throw new Error("cache migrate config must expose app");
    const appRoot = path.dirname(configFile);
    const configuredTarget = config.targets?.[platform] ?? {};
    const lockPath = path.join(appRoot, "src-tauri", "Cargo.lock");
    const identity = resolveSharedCacheIdentity({ cargoLockPath: lockPath, cargoTomlPath: path.join(appRoot, "src-tauri", "Cargo.toml"), rustcVerbose: env.RIGHT_RELEASE_RUSTC_VERBOSE, targetTriple: configuredTarget.cargoTarget ?? configuredTarget.targetTriple ?? configuredTarget.rustTarget ?? env.TAURI_ENV_TARGET_TRIPLE, profile: configuredTarget.profile ?? "release" });
    const fingerprint = env.RIGHT_RELEASE_CACHE_KEY || identity.fingerprint;
    const layout = resolveCacheLayout({ cacheRoot, platform, architecture: identity.architecture, app: config.app, fingerprint, kind: "release" });
    const dryRun = !rest.includes("--apply");
    const legacy = resolveReleaseLayout({ repoRoot: findRepoRoot(appRoot), configPath: configFile });
    const legacyTargetRoot = path.join(legacy.vaultRoot, "cache", "cargo-target", platform, fingerprint);
    const result = migrateLegacyCache({ legacyRoot: path.join(appRoot, "src-tauri", "target"), legacyTargetRoot, legacyCargoHome: path.join(legacy.vaultRoot, "cache", "cargo-home"), legacyReleaseLockPath: path.join(legacy.vaultRoot, "locks", `${platform}.lock.json`), layout, app: config.app, dryRun });
    return output({ schema: 1, command, cacheRoot, ...result }, json, stdout);
  }
  throw new Error("right-release cache: expected status, prune, or migrate");
}

function output(value, json, stdout) {
  if (json) stdout(JSON.stringify(value));
  else if (value.command === "status") stdout(`RightKit cache root: ${value.cacheRoot}\nTargets: ${value.targetBytes} bytes\nBackups: ${value.backupBytes} bytes\nFree: ${(value.cacheFreeBytes / 1024 ** 3).toFixed(1)} GiB\nLeases: ${value.entries.filter((entry) => entry.leased).length}`);
  else stdout(`RightKit cache ${value.command}: ${value.dryRun ? "dry-run" : "applied"}\nRoot: ${value.cacheRoot}\nCandidates: ${(value.candidateIds ?? value.plan?.candidateIds ?? []).join(", ") || "none"}\nReclaimed: ${value.reclaimedBytes ?? 0} bytes`);
  return value;
}

function option(args, name) { const index = args.indexOf(name); return index >= 0 ? args[index + 1] : undefined; }
function rejectUnknown(args, allowed) { for (const arg of args) if (!allowed.has(arg)) throw new Error(`unknown cache argument: ${arg}`); }
function platformFrom(env) { return env.RIGHT_RELEASE_PLATFORM || (process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform); }
function findRepoRoot(start) { let current = path.resolve(start); for (;;) { if (existsSync(path.join(current, ".git"))) return current; const parent = path.dirname(current); if (parent === current) throw new Error(`cache migrate could not find Git repository for ${start}`); current = parent; } }

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  runCacheCommand().catch((error) => { console.error(error.message); process.exitCode = 2; });
}
