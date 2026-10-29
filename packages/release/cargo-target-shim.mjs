#!/usr/bin/env node
// The runtime cargo wrapper of a commit-snapshot compile (item 7, GATE-CONTRACT §1 "Broker interface", output directory). Scope: the
// DIRECT POSIX (Linux) host only. On Mac and Dell the positive snapshot-compile path is the RustRight broker's snapshot carrier alone
// (the gate refuses a snapshot compile on a broker host until it is wired); on a direct Windows host the static walk
// (deterministic-scans.mjs cargoReachOf) is the fail-closed baseline, because a child started without a shell finds only an .exe.
//
// The gate puts this `cargo` first on PATH and names it in CARGO for every scan. Real cargo honours CARGO as its own path and hands
// it to build scripts and tests (env!("CARGO"), $CARGO), so those come back here too. Each invocation resolves its Cargo workspace
// (`cargo locate-project --workspace`, from its working directory or --manifest-path) and runs with that workspace's ORIGINAL checkout
// target directory as CARGO_TARGET_DIR. Refused with an RK_* code (the unit fails): a workspace the compile did not resolve (outside
// the snapshot, a fixture, a temporary project), an option that moves the workspace, configuration or output, a command that writes
// no bound output or rewrites the commit (clean, install, publish, update, an alias or plugin), a command that reads a workspace
// outside the bound ones or may write its Cargo.lock (tree, fetch, pkgid... without --locked/--frozen; fmt only as --check). A cargo
// that bypasses PATH and CARGO (an absolute path, `rustup run`, a reset PATH) inherits the scan-wide CARGO_TARGET_DIR: the one
// workspace's checkout target, or with several workspaces a per-scan sentinel directory that must stay empty (runScans fails the
// scan with RK_CARGO_UNBOUND otherwise). An absolute cargo that also passes its own --target-dir is deliberate evasion of the gate,
// out of scope under the standing rule (the gate binds what a test does, not what a test sets out to hide).
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const SHIM_MAP_SCHEMA = "rightkit.cargo-target-shim/2";
// Exit status of a refusal: cargo's own error status, so every caller reads it as a failed cargo.
export const SHIM_REFUSAL_STATUS = 101;
// Cargo commands that compile or report on a workspace: run bound to its checkout target directory.
const COMPILE = new Set(["build", "b", "check", "c", "test", "t", "bench", "run", "r", "doc", "d", "rustc", "rustdoc", "clippy", "metadata", "package"]);
// Commands that only read a workspace but may resolve and write its Cargo.lock: bound, and only with --locked or --frozen.
const LOCKED = new Set(["fetch", "tree", "pkgid", "verify-project", "read-manifest", "locate-project", "deny"]);
// Commands with no lock flag (cargo fmt, cargo-audit): bound, and only where the workspace's committed Cargo.lock already exists.
const LOCKFILE = new Set(["fmt", "audit"]);
// Plugins whose options are their own, not cargo's (cargo-deny's --config is its config file, not a cargo redirect).
const PLUGIN = new Set(["deny", "audit"]);
const BOUND = new Set([...COMPILE, ...LOCKED, ...LOCKFILE]);
// Commands that read no workspace at all: run unchanged.
const PASSTHROUGH = new Set(["version", "help"]);
// Global options that take a value (`--color always`, `--explain E0001`); the subcommand is the first word after them.
const GLOBAL_VALUE = new Set(["--color", "--explain"]);
// Options that move the working directory, the configuration or the output (a short cluster holding C or Z too).
const REDIRECT = /^(--config|-Z|-C)(=|$)|^-[A-Za-z]*[CZ]/;

const refusal = (code, message) => ({ refuse: { code, message } });

/**
 * What one cargo invocation may do: `{ exec: { args, env, workspace, targetDir } }` (bound), `{ exec: { args, env } }` (a command
 * that writes nothing), or `{ refuse: { code, message } }`. `locate(manifestPath | null, cwd, toolchain | null)` returns the absolute
 * path of the workspace root's Cargo.toml or throws; `realpath` resolves a directory. `map.shim` (the wrapper's own path) is set as
 * CARGO for the real cargo, so its build scripts and tests reach the wrapper again.
 */
export function decideCargoInvocation({ args, cwd, env, map, locate, realpath = (p) => fs.realpathSync.native(p), exists = fs.existsSync }) {
  const words = [...args];
  const toolchain = words[0]?.startsWith("+") ? words[0] : null;
  const withCargo = (e) => (map.shim ? { ...e, CARGO: map.shim } : e);
  let i = toolchain ? 1 : 0;
  let sub = null;
  for (; i < words.length; i += 1) {
    const w = words[i];
    if (w === "--") break;
    if (REDIRECT.test(w)) return refusal("RK_CARGO_REDIRECT", `\`cargo ${words.join(" ")}\` passes ${w}, which moves its working directory, configuration or output; a snapshot compile's outputs must be the checkout's`);
    if (GLOBAL_VALUE.has(w)) { i += 1; continue; }
    if (w.startsWith("-")) continue;
    sub = w;
    break;
  }
  const end = sub === null ? -1 : words.indexOf("--", i + 1);
  const own = sub === null ? [] : words.slice(i + 1, end < 0 ? undefined : end);
  const globals = words.slice(toolchain ? 1 : 0, sub === null ? undefined : i);
  if (sub === "fmt" && !own.includes("--check")) return refusal("RK_CARGO_UNBOUND_COMMAND", "`cargo fmt` without --check rewrites tracked files; a snapshot compile changes no file of the commit");
  if (sub === null || PASSTHROUGH.has(sub)) return { exec: { args: words, env: withCargo({ ...env }) } };
  if (!BOUND.has(sub)) return refusal("RK_CARGO_UNBOUND_COMMAND", `\`cargo ${sub}\` is not a command a snapshot compile binds (${[...BOUND].join(", ")}; or ${[...PASSTHROUGH].join(", ")}, which read no workspace): an alias, plugin, clean, install, publish or lock rewrite is refused`);
  if (LOCKED.has(sub) && ![...globals, ...own].some((w) => w === "--locked" || w === "--frozen")) return refusal("RK_CARGO_UNBOUND_COMMAND", `\`cargo ${sub}\` without --locked or --frozen may write a Cargo.lock; a snapshot compile changes no file of the commit and writes nothing outside its bound outputs`);
  if (!PLUGIN.has(sub)) for (const w of own) if (REDIRECT.test(w)) return refusal("RK_CARGO_REDIRECT", `\`cargo ${sub}\` passes ${w}, which moves its working directory, configuration or output; a snapshot compile's outputs must be the checkout's`);
  const valueOf = (name) => {
    const at = own.findIndex((w) => w === name || w.startsWith(`${name}=`));
    if (at < 0) return undefined;
    return own[at].includes("=") ? own[at].slice(name.length + 1) : (own[at + 1] ?? "");
  };
  const manifestPath = valueOf("--manifest-path");
  let workspace;
  try {
    const manifest = locate(manifestPath === undefined ? null : path.resolve(cwd, manifestPath), cwd, toolchain);
    if (typeof manifest !== "string" || !path.isAbsolute(manifest)) throw new Error(`locate-project reported ${JSON.stringify(manifest)}`);
    workspace = realpath(path.dirname(manifest));
  } catch (error) {
    return refusal("RK_CARGO_UNBOUND_WORKSPACE", `\`cargo ${sub}\` from ${cwd}${manifestPath === undefined ? "" : ` (--manifest-path ${manifestPath})`}: its Cargo workspace cannot be resolved (${error.message}); a snapshot compile's outputs must be the checkout's`);
  }
  const targetDir = Object.hasOwn(map.workspaces ?? {}, workspace) ? map.workspaces[workspace] : null;
  if (!targetDir) return refusal("RK_CARGO_UNBOUND_WORKSPACE", `\`cargo ${sub}\` builds the Cargo workspace ${workspace}, which is not one this snapshot compile resolved (${Object.keys(map.workspaces ?? {}).join(", ") || "none"}); its outputs would not be the checkout's`);
  if (LOCKFILE.has(sub) && !exists(path.join(workspace, "Cargo.lock"))) return refusal("RK_CARGO_UNBOUND_COMMAND", `\`cargo ${sub}\` has no --locked, and workspace ${workspace} has no committed Cargo.lock it would only read; it may write one`);
  const explicit = valueOf("--target-dir");
  if (explicit !== undefined && path.resolve(cwd, explicit) !== targetDir) return refusal("RK_CARGO_REDIRECT", `\`cargo ${sub}\` passes --target-dir ${explicit}; workspace ${workspace} writes only to the checkout's ${targetDir}`);
  return { exec: { args: words, env: withCargo({ ...env, CARGO_TARGET_DIR: targetDir, CARGO_BUILD_TARGET_DIR: targetDir }), workspace, targetDir } };
}

/**
 * Writes the wrapper for one snapshot compile under `base`: the map file and `bin/cargo`, which runs this module with node. The
 * directory is named by the sha256 of the map and this module's bytes (`identity`), so the same workspaces, targets and wrapper give
 * the same path (and the same CARGO in a scan's cache key) and any change gives another. `targetDirs` is the gate's { [cargoDir]:
 * absolute checkout target directory }; each workspace is keyed by its real path in the snapshot. `realCargo` is the absolute cargo
 * the wrapper runs (never itself). POSIX hosts only: a Windows child started without a shell finds only an .exe on PATH.
 */
export function installCargoShim({ base, snapshotRoot, targetDirs, realCargo, node = process.execPath, platform = process.platform, realpath = (p) => fs.realpathSync.native(p), onDir = () => {}, write = (file, text, mode) => fs.writeFileSync(file, text, { mode }) }) {
  if (platform === "win32") throw Object.assign(new Error("the runtime cargo wrapper needs a POSIX host: a Windows child finds only an .exe on PATH"), { code: "RK_CARGO_SHIM_UNSUPPORTED" });
  if (typeof realCargo !== "string" || !path.isAbsolute(realCargo)) throw Object.assign(new Error(`no absolute cargo to wrap (${JSON.stringify(realCargo)})`), { code: "RK_CARGO_SHIM_UNSUPPORTED" });
  const workspaces = {};
  for (const [cargoDir, targetDir] of Object.entries(targetDirs ?? {})) workspaces[realpath(path.join(snapshotRoot, ...(cargoDir === "." ? [] : cargoDir.split("/"))))] = targetDir;
  const self = fileURLToPath(import.meta.url);
  const sorted = Object.fromEntries(Object.entries(workspaces).sort(([a], [b]) => (a < b ? -1 : 1)));
  const identity = createHash("sha256").update(JSON.stringify({ realCargo, node, workspaces: sorted })).update(fs.readFileSync(self)).digest("hex");
  const dir = path.join(base, `rk-cargo-shim-${identity.slice(0, 16)}`);
  const bin = path.join(dir, "bin");
  const shim = path.join(bin, "cargo");
  const mapFile = path.join(dir, "cargo-target-map.json");
  // The caller learns the directory before anything is written into it (its cleanup owns it from here), and a failed install removes
  // what it wrote before it throws.
  onDir(dir);
  try {
    fs.mkdirSync(bin, { recursive: true });
    const quote = (s) => `'${String(s).replaceAll("'", "'\\''")}'`;
    const put = (file, text, mode) => { const tmp = `${file}.${process.pid}.tmp`; write(tmp, text, mode); fs.renameSync(tmp, file); };
    put(mapFile, `${JSON.stringify({ schema: SHIM_MAP_SCHEMA, identity, realCargo, shim, workspaces: sorted }, null, 2)}\n`, 0o644);
    put(shim, `#!/bin/sh\nexec ${quote(node)} ${quote(self)} --map ${quote(mapFile)} -- "$@"\n`, 0o755);
  } catch (error) {
    fs.rmSync(dir, { recursive: true, force: true });
    throw error;
  }
  return { dir, bin, shim, mapFile, workspaces: sorted, identity, sentinel: Object.keys(sorted).length > 1 ? path.join(dir, "unbound") : null };
}

/**
 * The environment every scan of a wrapped snapshot compile starts from: the wrapper first on PATH and named in CARGO, and, when the
 * snapshot has exactly one Cargo workspace, its checkout target as the scan-wide CARGO_TARGET_DIR, so a cargo that bypasses the
 * wrapper (an absolute path, `rustup run`, a reset PATH) still writes there. With several workspaces runScans gives each scan its own
 * empty sentinel as CARGO_TARGET_DIR instead (installed.sentinel; options.cargoSentinel), which must still be empty afterwards.
 */
export function shimScanEnv(env, installed) {
  const targets = Object.values(installed.workspaces);
  const pathKey = Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "PATH";
  return { ...env, [pathKey]: `${installed.bin}${path.delimiter}${env[pathKey] ?? ""}`, CARGO: installed.shim, ...(targets.length === 1 ? { CARGO_TARGET_DIR: targets[0] } : {}) };
}

/** The wrapper itself: `cargo-target-shim.mjs --map <file> -- <cargo args>`. Returns the exit status. */
export function runShim(argv, { env = process.env, cwd = process.cwd(), spawn = spawnSync, err = (m) => process.stderr.write(`${m}\n`) } = {}) {
  const sep = argv.indexOf("--");
  const mapAt = argv.indexOf("--map");
  if (mapAt < 0 || sep < 0 || mapAt > sep) { err("rightkit-gate: RK_CARGO_SHIM_ERROR: usage: cargo-target-shim.mjs --map <file> -- <cargo args>"); return SHIM_REFUSAL_STATUS; }
  let map;
  try { map = JSON.parse(fs.readFileSync(argv[mapAt + 1], "utf8")); if (map?.schema !== SHIM_MAP_SCHEMA) throw new Error(`schema ${JSON.stringify(map?.schema)}`); }
  catch (error) { err(`rightkit-gate: RK_CARGO_SHIM_ERROR: the cargo target map cannot be read (${error.message}); nothing ran`); return SHIM_REFUSAL_STATUS; }
  // locate-project runs the real cargo with CARGO pointing at it, never at the wrapper.
  const locateEnv = { ...env, CARGO: map.realCargo };
  const locate = (manifestPath, from, toolchain) => {
    const r = spawn(map.realCargo, [...(toolchain ? [toolchain] : []), "locate-project", "--workspace", "--message-format", "plain", ...(manifestPath ? ["--manifest-path", manifestPath] : [])], { cwd: from, env: locateEnv, encoding: "utf8" });
    if (r.error) throw r.error;
    if (r.status !== 0) throw new Error(String(r.stderr || `exit ${r.status}`).trim().split(/\r?\n/).at(-1));
    return String(r.stdout).trim();
  };
  const decision = decideCargoInvocation({ args: argv.slice(sep + 1), cwd, env, map, locate });
  if (decision.refuse) { err(`rightkit-gate: ${decision.refuse.code}: ${decision.refuse.message}`); return SHIM_REFUSAL_STATUS; }
  const r = spawn(map.realCargo, decision.exec.args, { cwd, env: decision.exec.env, stdio: "inherit" });
  if (r.error) { err(`rightkit-gate: RK_CARGO_SHIM_ERROR: ${map.realCargo} did not start (${r.error.message})`); return SHIM_REFUSAL_STATUS; }
  return r.status ?? (r.signal ? 128 : SHIM_REFUSAL_STATUS);
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) process.exit(runShim(process.argv.slice(2)));
