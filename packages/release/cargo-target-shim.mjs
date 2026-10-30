#!/usr/bin/env node
// The runtime cargo wrapper of a commit-snapshot compile (item 7, GATE-CONTRACT §1 "Broker interface", output directory). Two modes:
//   direct  the DIRECT POSIX (Linux) host: each call runs the real cargo bound to its workspace's checkout target directory.
//   broker  the Mac (decision C, rows 7.10 and 7.24): each call goes through the RustRight client as its own snapshot request with a
//           fresh carrier (snapshot-broker.mjs runBrokerCargo); no cargo ever runs outside the broker, and its signed receipt must verify.
// Both modes append every call to the gate-owned ledger (cargo-ledger.mjs) before and after it acts, and the gate fails the scan on any
// refused, unverified or unfinished call even when the test ignored the exit status. On a Windows host there is no wrapper: a child
// started without a shell finds only an .exe, so the static walk (deterministic-scans.mjs cargoReachOf) is the fail-closed baseline
// there (a refused unit is FAIL or BLOCKED with a code, never skipped).
//
// The gate puts this `cargo` first on PATH and names it in CARGO for every scan. A test, script or build tool that runs `cargo` or
// $CARGO comes back here, with one accepted exception (lead re-decision F7, row 7.10). MEASURED, cargo 1.98.0 on WSL2
// (/Volumes/D/scratch/rk-l7b/wsl-debug1.sh, wsl-debug3.sh): a real cargo hands a build script its OWN path in CARGO, not the file
// the CARGO variable names, and `--config env.CARGO` does not change that. A cargo a build script starts through $CARGO therefore
// bypasses this wrapper. It is accepted as follows: it inherits the parent's bound CARGO_TARGET_DIR (the same as a primary-checkout
// build), so its outputs land in the bound checkout target; the per-scan sentinel and the after-compile snapshot verify stay as
// backstops; and it is UNOBSERVED: never in the ledger, and every scan result of a wrapped compile carries a note that says so
// (deterministic-scans.mjs UNOBSERVED_NESTED_CARGO_NOTE). Each invocation resolves its Cargo workspace
// (`cargo locate-project --workspace`, from its working directory or --manifest-path) and runs with that workspace's ORIGINAL checkout
// target directory as CARGO_TARGET_DIR. Refused with an RK_* code (the unit fails): a workspace the compile did not resolve (outside
// the snapshot, a fixture, a temporary project), an option that moves the workspace, configuration or output, a command that writes
// no bound output or rewrites the commit (clean, install, publish, update, an alias or plugin), a command that reads a workspace
// outside the bound ones or may write its Cargo.lock (tree, fetch, pkgid... without --locked/--frozen; fmt only as --check). A cargo
// that bypasses PATH and CARGO (an absolute path, `rustup run`, a reset PATH) inherits the scan-wide CARGO_TARGET_DIR: the one
// workspace's checkout target, or with several workspaces a per-scan sentinel directory that must stay empty (runScans fails the
// scan with RK_CARGO_UNBOUND otherwise). In broker mode the sentinel is always set: there is no checkout target to inherit, so any
// write into it is a cargo that skipped the wrapper. An absolute cargo that also passes its own absolute target directory has
// overridden the value the gate passed: a residual of the trusted per-user model, not refused (map 7.12).
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { spawnBrokerClient } from "./broker-client.mjs";
import { fileURLToPath } from "node:url";
import { SCAN_TOKEN_ENV, appendLedger } from "./cargo-ledger.mjs";
import { BROKER_REFUSAL_STATUS, runBrokerCargo } from "./snapshot-broker.mjs";

export const SHIM_MAP_SCHEMA = "rightkit.cargo-target-shim/3";
// Exit status of a refusal: cargo's own error status, so every caller reads it as a failed cargo.
export const SHIM_REFUSAL_STATUS = BROKER_REFUSAL_STATUS;
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
/** The subcommands the wrapper binds or passes through (the contract test pins each to the broker's classification). */
export const WRAPPER_COMMANDS = Object.freeze({ get bound() { return [...BOUND]; }, get passthrough() { return [...PASSTHROUGH]; } });
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
  const broker = map.mode === "broker";
  // The RustRight client submits the call from the working directory and refuses to rewrite it (attestation), and the broker pins the
  // output: a manifest path or a target directory of the caller's has no place in a broker call.
  if (broker && manifestPath !== undefined) return refusal("RK_CARGO_BROKER_MANIFEST_PATH", `\`cargo ${sub}\` passes --manifest-path; a broker call runs from the workspace's own directory (the client refuses to rewrite the invocation), so run it there`);
  if (broker && valueOf("--target-dir") !== undefined) return refusal("RK_CARGO_REDIRECT", `\`cargo ${sub}\` passes --target-dir; the broker pins a snapshot compile's output directory`);
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
  // Broker mode: `targetDir` is the workspace's counterpart in the primary checkout (the gate's snapshot-to-checkout map).
  if (broker) return { exec: { args: words, env: { ...env }, workspace, checkoutWorkspace: targetDir } };
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
 *
 * Broker mode (`broker: { checkoutDirs, section }`, the Mac): `checkoutDirs` is the gate's { [cargoDir]: absolute path of that
 * workspace in the primary checkout } (the snapshot-to-checkout map, the only source of a request's primaryCwd) and `section` the
 * public part of the gate's snapshot broker (brokerSection). `realCargo` then only runs `version` and `help`; it may be null.
 */
export function installCargoShim({ base, snapshotRoot, targetDirs, broker = null, realCargo, node = process.execPath, platform = process.platform, realpath = (p) => fs.realpathSync.native(p), onDir = () => {}, write = (file, text, mode) => fs.writeFileSync(file, text, { mode }) }) {
  if (platform === "win32") throw Object.assign(new Error("the runtime cargo wrapper needs a POSIX host: a Windows child finds only an .exe on PATH"), { code: "RK_CARGO_SHIM_UNSUPPORTED" });
  const mode = broker ? "broker" : "direct";
  if (mode === "direct" && (typeof realCargo !== "string" || !path.isAbsolute(realCargo))) throw Object.assign(new Error(`no absolute cargo to wrap (${JSON.stringify(realCargo)})`), { code: "RK_CARGO_SHIM_UNSUPPORTED" });
  if (mode === "broker" && ((realCargo !== null && realCargo !== undefined && !path.isAbsolute(realCargo)) || !broker.section || !broker.checkoutDirs)) throw Object.assign(new Error("a broker wrapper needs the gate's checkout map and broker section"), { code: "RK_CARGO_SHIM_UNSUPPORTED" });
  const workspaces = {};
  for (const [cargoDir, targetDir] of Object.entries(broker ? broker.checkoutDirs : targetDirs ?? {})) workspaces[realpath(path.join(snapshotRoot, ...(cargoDir === "." ? [] : cargoDir.split("/"))))] = targetDir;
  const self = fileURLToPath(import.meta.url);
  const sorted = Object.fromEntries(Object.entries(workspaces).sort(([a], [b]) => (a < b ? -1 : 1)));
  const identityHash = createHash("sha256").update(JSON.stringify({ realCargo: realCargo ?? null, node, workspaces: sorted, ...(broker ? { mode, section: broker.section } : {}) })).update(fs.readFileSync(self));
  // The broker half of the wrapper is part of its identity: any change to the code that carries and verifies a call is another wrapper.
  if (broker) for (const name of ["snapshot-broker.mjs", "snapshot-request.mjs", "provenance-verify.mjs", "cargo-ledger.mjs"]) identityHash.update(fs.readFileSync(new URL(`./${name}`, import.meta.url)));
  const identity = identityHash.digest("hex");
  const dir = path.join(base, `rk-cargo-shim-${identity.slice(0, 16)}`);
  const bin = path.join(dir, "bin");
  const shim = path.join(bin, "cargo");
  const mapFile = path.join(dir, "cargo-target-map.json");
  const ledgerDir = path.join(dir, "ledger");
  // The caller learns the directory before anything is written into it (its cleanup owns it from here), and a failed install removes
  // what it wrote before it throws.
  onDir(dir);
  try {
    fs.mkdirSync(bin, { recursive: true });
    fs.mkdirSync(ledgerDir, { recursive: true, mode: 0o700 });
    const quote = (s) => `'${String(s).replaceAll("'", "'\\''")}'`;
    const put = (file, text, mode) => { const tmp = `${file}.${process.pid}.tmp`; write(tmp, text, mode); fs.renameSync(tmp, file); };
    const sentinel = mode === "broker" || Object.keys(sorted).length > 1 ? path.join(dir, "unbound") : null;
    const section = broker ? { ...broker.section, sentinel } : null;
    put(mapFile, `${JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode, identity, realCargo: realCargo ?? null, shim, ledgerDir, workspaces: sorted, ...(section ? { broker: section } : {}) }, null, 2)}\n`, 0o644);
    put(shim, `#!/bin/sh\nexec ${quote(node)} ${quote(self)} --map ${quote(mapFile)} -- "$@"\n`, 0o755);
    return { dir, bin, shim, mapFile, ledgerDir, mode, workspaces: sorted, identity, sentinel };
  } catch (error) {
    fs.rmSync(dir, { recursive: true, force: true });
    throw error;
  }
}

/**
 * The environment every scan of a wrapped snapshot compile starts from: the wrapper first on PATH and named in CARGO, and, when the
 * snapshot has exactly one Cargo workspace (direct mode), its checkout target as the scan-wide CARGO_TARGET_DIR, so a cargo that
 * bypasses the wrapper (an absolute path, `rustup run`, a reset PATH) still writes there. Otherwise runScans gives each scan its own
 * empty sentinel as CARGO_TARGET_DIR instead (installed.sentinel; options.cargoSentinel), which must still be empty afterwards.
 * Broker mode has no checkout target to inherit and always uses the sentinel.
 */
export function shimScanEnv(env, installed) {
  const targets = Object.values(installed.workspaces);
  const pathKey = Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "PATH";
  return { ...env, [pathKey]: `${installed.bin}${path.delimiter}${env[pathKey] ?? ""}`, CARGO: installed.shim, ...(installed.mode !== "broker" && targets.length === 1 ? { CARGO_TARGET_DIR: targets[0] } : {}) };
}

/**
 * The workspace root Cargo resolves for a manifest or a directory, from the files alone (broker mode: no cargo runs outside the
 * broker, not even `locate-project`): the nearest enclosing Cargo.toml that declares `[workspace]`, else the nearest Cargo.toml.
 */
export function locateWorkspaceByWalk(manifestPath, cwd, { read = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync } = {}) {
  let nearest = null;
  for (let dir = manifestPath ? path.dirname(manifestPath) : cwd; ; dir = path.dirname(dir)) {
    const manifest = path.join(dir, "Cargo.toml");
    if (exists(manifest)) {
      nearest ??= manifest;
      let text = "";
      try { text = read(manifest); } catch { text = ""; }
      if (/^\s*\[workspace(?:\.[^\]]*)?\]/m.test(text)) return manifest;
    }
    if (path.dirname(dir) === dir) break;
  }
  if (nearest) return nearest;
  throw new Error("could not find `Cargo.toml` in the working directory or any parent");
}

/** The wrapper itself: `cargo-target-shim.mjs --map <file> -- <cargo args>`. Returns the exit status. */
export function runShim(argv, { env = process.env, cwd = process.cwd(), spawn = spawnSync, err = (m) => process.stderr.write(`${m}\n`) } = {}) {
  const sep = argv.indexOf("--");
  const mapAt = argv.indexOf("--map");
  if (mapAt < 0 || sep < 0 || mapAt > sep) { err("rightkit-gate: RK_CARGO_SHIM_ERROR: usage: cargo-target-shim.mjs --map <file> -- <cargo args>"); return SHIM_REFUSAL_STATUS; }
  let map;
  try { map = JSON.parse(fs.readFileSync(argv[mapAt + 1], "utf8")); if (map?.schema !== SHIM_MAP_SCHEMA) throw new Error(`schema ${JSON.stringify(map?.schema)}`); }
  catch (error) { err(`rightkit-gate: RK_CARGO_SHIM_ERROR: the cargo target map cannot be read (${error.message}); nothing ran`); return SHIM_REFUSAL_STATUS; }
  const words = argv.slice(sep + 1);
  const token = env[SCAN_TOKEN_ENV];
  const id = `${process.pid}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  const broker = map.mode === "broker";
  const base = { id, mode: broker ? "broker" : "direct", cwd, argv: words };
  // A call that cannot be recorded does not run: the ledger is how the gate learns what a test did, whatever the test does with the status.
  const record = (entry) => appendLedger(map.ledgerDir, token, entry);
  try { record({ ...base, status: "pending" }); } catch (error) { err(`rightkit-gate: RK_CARGO_LEDGER_ERROR: the call cannot be recorded (${error.message}); it does not run`); return SHIM_REFUSAL_STATUS; }
  const refuse = (why) => { record({ ...base, status: "refused", code: why.code, message: why.message }); err(`rightkit-gate: ${why.code}: ${why.message}`); return SHIM_REFUSAL_STATUS; };
  // locate-project runs the real cargo with CARGO pointing at it, never at the wrapper (direct mode). Broker mode reads the files only.
  const locateEnv = { ...env, CARGO: map.realCargo };
  const locate = broker ? (manifestPath, from) => locateWorkspaceByWalk(manifestPath, from) : (manifestPath, from, toolchain) => {
    const r = spawn(map.realCargo, [...(toolchain ? [toolchain] : []), "locate-project", "--workspace", "--message-format", "plain", ...(manifestPath ? ["--manifest-path", manifestPath] : [])], { cwd: from, env: locateEnv, encoding: "utf8" });
    if (r.error) throw r.error;
    if (r.status !== 0) throw new Error(String(r.stderr || `exit ${r.status}`).trim().split(/\r?\n/).at(-1));
    return String(r.stdout).trim();
  };
  let decision;
  try { decision = decideCargoInvocation({ args: words, cwd, env, map, locate }); } catch (error) { return refuse({ code: "RK_CARGO_SHIM_ERROR", message: error.message }); }
  if (decision.refuse) return refuse(decision.refuse);
  if (broker && decision.exec.workspace) {
    // The `pending` record above stays this call's until the broker call finishes: its records carry the same call id.
    return runBrokerCargo({ words: decision.exec.args, cwd, env, section: map.broker, workspace: decision.exec.workspace, checkoutWorkspace: decision.exec.checkoutWorkspace, record: (entry) => record({ ...entry, id }), spawn, err });
  }
  // A call that reads no workspace (version, help) runs as it is: the real cargo in direct mode, the host's cargo or the client in broker mode.
  const viaClient = broker && !map.realCargo;
  const command = viaClient ? map.broker.client : map.realCargo;
  const spawnOpts = { cwd, env: decision.exec.env, stdio: "inherit" };
  const r = viaClient ? spawnBrokerClient(spawn, command, ["cargo", ...decision.exec.args], spawnOpts, { platform: map.broker.platform }) : spawn(command, decision.exec.args, spawnOpts);
  if (r.error) return refuse({ code: "RK_CARGO_SHIM_ERROR", message: `${command} did not start (${r.error.message})` });
  record({ ...base, status: "ran", exit: r.status ?? null });
  return r.status ?? (r.signal ? 128 : SHIM_REFUSAL_STATUS);
}

// Run as the wrapper only when this file is the entry. Imported by another module (carried-cargo.mjs, through managed-cargo.mjs), the entry
// may be anything, `node -e` arguments included, so a path that cannot be resolved is no wrapper run.
const invokedAsWrapper = (() => { try { return Boolean(process.argv[1]) && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })();
if (invokedAsWrapper) process.exit(runShim(process.argv.slice(2)));
