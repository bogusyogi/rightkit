#!/usr/bin/env node
// The runtime cargo wrapper of a commit-snapshot compile (item 7, GATE-CONTRACT §1 "Broker interface", output directory): on a direct
// (Linux) host the gate puts a `cargo` of its own first on PATH for every scan. Each real cargo invocation, however a test reaches it
// (a script, `node --test`, a build helper), resolves its Cargo workspace (`cargo locate-project --workspace`, from its working
// directory or --manifest-path) and runs with that workspace's ORIGINAL checkout target directory as CARGO_TARGET_DIR. A workspace the
// snapshot compile did not resolve (outside the snapshot, a fixture, a temporary project), a cargo command that writes no bound
// output or rewrites the commit, and an option that moves the workspace or output are refused with an RK_* code: the unit fails.
// The static walker (deterministic-scans.mjs cargoReachOf) is advisory where this wrapper is installed.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const SHIM_MAP_SCHEMA = "rightkit.cargo-target-shim/1";
// Exit status of a refusal: cargo's own error status, so every caller reads it as a failed cargo.
export const SHIM_REFUSAL_STATUS = 101;
// Cargo commands that compile or report outputs: run bound to their workspace's checkout target directory.
const BOUND = new Set(["build", "b", "check", "c", "test", "t", "bench", "run", "r", "doc", "d", "rustc", "rustdoc", "clippy", "metadata", "package"]);
// Cargo commands that read the workspace and write no compile output (deny and audit: the gate's own class A scans): run unchanged,
// no workspace needed.
const PASSTHROUGH = new Set(["version", "help", "locate-project", "verify-project", "pkgid", "read-manifest", "tree", "fetch", "fmt", "deny", "audit"]);
// Global options that take a value (`--color always`, `--explain E0001`); the subcommand is the first word after them.
const GLOBAL_VALUE = new Set(["--color", "--explain"]);
// Options that move the working directory, the configuration or the output (a short cluster holding C or Z too).
const REDIRECT = /^(--config|-Z|-C)(=|$)|^-[A-Za-z]*[CZ]/;

const refusal = (code, message) => ({ refuse: { code, message } });

/**
 * What one cargo invocation may do: `{ exec: { args, env, workspace, targetDir } }` (bound), `{ exec: { args, env } }` (a command
 * with no compile output), or `{ refuse: { code, message } }`. `locate(manifestPath | null, cwd, toolchain | null)` returns the
 * absolute path of the workspace root's Cargo.toml or throws; `realpath` resolves a directory.
 */
export function decideCargoInvocation({ args, cwd, env, map, locate, realpath = (p) => fs.realpathSync.native(p) }) {
  const words = [...args];
  const toolchain = words[0]?.startsWith("+") ? words[0] : null;
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
  if (sub === null || PASSTHROUGH.has(sub)) return { exec: { args: words, env: { ...env } } };
  if (!BOUND.has(sub)) return refusal("RK_CARGO_UNBOUND_COMMAND", `\`cargo ${sub}\` is not a command a snapshot compile binds (${[...BOUND].join(", ")}; or ${[...PASSTHROUGH].join(", ")}, which write no output): an alias, plugin, clean, install, publish or lock rewrite is refused`);
  const own = words.slice(i + 1, words.includes("--", i + 1) ? words.indexOf("--", i + 1) : undefined);
  for (const w of own) if (REDIRECT.test(w)) return refusal("RK_CARGO_REDIRECT", `\`cargo ${sub}\` passes ${w}, which moves its working directory, configuration or output; a snapshot compile's outputs must be the checkout's`);
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
  const explicit = valueOf("--target-dir");
  if (explicit !== undefined && path.resolve(cwd, explicit) !== targetDir) return refusal("RK_CARGO_REDIRECT", `\`cargo ${sub}\` passes --target-dir ${explicit}; workspace ${workspace} writes only to the checkout's ${targetDir}`);
  return { exec: { args: words, env: { ...env, CARGO_TARGET_DIR: targetDir, CARGO_BUILD_TARGET_DIR: targetDir }, workspace, targetDir } };
}

/**
 * Writes the wrapper for one snapshot compile into `dir` (a fresh directory of the gate's own): the map file and `bin/cargo`, which
 * runs this module with node. `targetDirs` is the gate's { [cargoDir]: absolute checkout target directory }; each workspace is keyed
 * by its real path in the snapshot. `realCargo` is the absolute cargo the wrapper runs (never itself). POSIX hosts only: a Windows
 * child started without a shell finds only an .exe on PATH, so a script there would be bypassed.
 */
export function installCargoShim({ dir, snapshotRoot, targetDirs, realCargo, node = process.execPath, platform = process.platform, realpath = (p) => fs.realpathSync.native(p) }) {
  if (platform === "win32") throw Object.assign(new Error("the runtime cargo wrapper needs a POSIX host: a Windows child finds only an .exe on PATH"), { code: "RK_CARGO_SHIM_UNSUPPORTED" });
  if (typeof realCargo !== "string" || !path.isAbsolute(realCargo)) throw Object.assign(new Error(`no absolute cargo to wrap (${JSON.stringify(realCargo)})`), { code: "RK_CARGO_SHIM_UNSUPPORTED" });
  const workspaces = {};
  for (const [cargoDir, targetDir] of Object.entries(targetDirs ?? {})) workspaces[realpath(path.join(snapshotRoot, ...(cargoDir === "." ? [] : cargoDir.split("/"))))] = targetDir;
  const bin = path.join(dir, "bin");
  fs.mkdirSync(bin, { recursive: true });
  const mapFile = path.join(dir, "cargo-target-map.json");
  fs.writeFileSync(mapFile, `${JSON.stringify({ schema: SHIM_MAP_SCHEMA, realCargo, workspaces }, null, 2)}\n`);
  const quote = (s) => `'${String(s).replaceAll("'", "'\\''")}'`;
  const shim = path.join(bin, "cargo");
  fs.writeFileSync(shim, `#!/bin/sh\nexec ${quote(node)} ${quote(fileURLToPath(import.meta.url))} --map ${quote(mapFile)} -- "$@"\n`);
  fs.chmodSync(shim, 0o755);
  return { bin, mapFile, workspaces };
}

/** The wrapper itself: `cargo-target-shim.mjs --map <file> -- <cargo args>`. Returns the exit status. */
export function runShim(argv, { env = process.env, cwd = process.cwd(), spawn = spawnSync, err = (m) => process.stderr.write(`${m}\n`) } = {}) {
  const sep = argv.indexOf("--");
  const mapAt = argv.indexOf("--map");
  if (mapAt < 0 || sep < 0 || mapAt > sep) { err("rightkit-gate: RK_CARGO_SHIM_ERROR: usage: cargo-target-shim.mjs --map <file> -- <cargo args>"); return SHIM_REFUSAL_STATUS; }
  let map;
  try { map = JSON.parse(fs.readFileSync(argv[mapAt + 1], "utf8")); if (map?.schema !== SHIM_MAP_SCHEMA) throw new Error(`schema ${JSON.stringify(map?.schema)}`); }
  catch (error) { err(`rightkit-gate: RK_CARGO_SHIM_ERROR: the cargo target map cannot be read (${error.message}); nothing ran`); return SHIM_REFUSAL_STATUS; }
  const locate = (manifestPath, from, toolchain) => {
    const r = spawn(map.realCargo, [...(toolchain ? [toolchain] : []), "locate-project", "--workspace", "--message-format", "plain", ...(manifestPath ? ["--manifest-path", manifestPath] : [])], { cwd: from, env, encoding: "utf8" });
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
