// Class A launch guard: class A runs before any compile, so a class A test row may not launch a compiler or an emitting build.
//
// The gate runs every class A test row with NODE_OPTIONS carrying `--import=<this file>`. In every Node process of the row (the
// test runner, pnpm, workers, any Node child they launch, including one given a replacement environment) the child_process launchers are wrapped, and a launch is
// refused (it throws, so the row fails) when the program, or a command line it hands to a shell, is a compile or an emitting
// build: cargo, rustc, rustright, rightkit (broker client), tauri / cargo-tauri, tsc without --noEmit, tsup, esbuild, swc, rollup,
// webpack, `vite build`, `next build`, or a package-manager run of a build / build:* script.
//
// Scope, exactly: launches from Node processes of the row. Not covered: a compiler API called inside a process
// (typescript.createProgram().emit(), vite.build() / the rollup JS API), and launches by a non-Node program that does not load
// this hook (rightkit#1240). esbuild's JS API is covered: it launches the esbuild service binary.
//
// A Node child handed a replacement environment keeps the guard (withGuard restores it). There is no exemption: a unit whose tests
// launch stand-ins named like compilers (stub scripts can run anything) runs after class A instead (npm.<unit>.afterClassA).
import childProcess from "node:child_process";
import path from "node:path";
import { syncBuiltinESMExports } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

const INSTALLED = Symbol.for("rightkit.classA.launchGuard");
export const LAUNCH_GUARD_IMPORT = `--import=${pathToFileURL(fileURLToPath(import.meta.url)).href}`;
export const REFUSAL_CODE = "RK_CLASS_A_COMPILE_LAUNCH";
const COMPILERS = new Set(["cargo", "rustc", "rustright", "rightkit", "tauri", "cargo-tauri", "tsup", "esbuild", "swc", "rollup", "webpack"]);
const PACKAGE_MANAGERS = new Set(["pnpm", "npm", "yarn", "bun"]);
const SHELLS = new Set(["sh", "bash", "zsh", "dash", "cmd", "powershell", "pwsh"]);
const WRAPPERS = new Set(["env", "npx", "pnpx", "exec", "time", "nice"]);
const OPTIONS_WITH_VALUES = new Set(["--filter", "-F", "--dir", "-C", "--workspace", "-w", "--prefix"]);

const base = (p) => path.basename(String(p ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat|ps1|js|mjs|cjs)$/i, "").toLowerCase();

/** A shell command line split into commands (on && || ; |) and words, quotes removed: enough to read what a `sh -c` text launches. */
export function shellWords(text) {
  const commands = []; let words = []; let word = ""; let quote = null;
  const end = () => { if (word) { words.push(word); word = ""; } };
  const s = String(text);
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (quote) { if (c === quote) quote = null; else word += c; continue; }
    if (c === "'" || c === "\"") { quote = c; continue; }
    if (/\s/.test(c)) { end(); continue; }
    if (c === ";" || c === "|" || c === "&") { end(); if (words.length) commands.push(words); words = []; continue; }
    word += c;
  }
  end(); if (words.length) commands.push(words);
  return commands;
}

/** Why `[program, ...args]` is a compile or an emitting build, or null. */
export function compileLaunch(program, args = []) {
  let words = [String(program), ...args.map(String)];
  for (let guard = 0; guard < 8 && words.length; guard++) {
    const name = base(words[0]);
    if (name === "node" && words[1] && !String(words[1]).startsWith("-")) { words = words.slice(1); continue; }
    if (WRAPPERS.has(name)) { words = words.slice(1).filter((w, i, a) => !(i === 0 && /^[A-Z_][A-Z0-9_]*=/.test(w))); continue; }
    if (SHELLS.has(name)) {
      const at = words.findIndex((w, i) => i > 0 && /^(-c|\/c|\/s|-Command|-command)$/i.test(w));
      if (at < 0 || !words[at + 1]) return null;
      for (const cmd of shellWords(words.slice(at + 1).join(" "))) { const why = compileLaunch(cmd[0], cmd.slice(1)); if (why) return why; }
      return null;
    }
    const rest = words.slice(1);
    if (COMPILERS.has(name)) return name;
    if (name === "tsc") return rest.some((w) => /^--?noEmit(=true)?$/i.test(w)) ? null : "tsc (emits)";
    if (name === "vite" || name === "next") return rest.find((w) => !w.startsWith("-")) === "build" ? `${name} build` : null;
    if (PACKAGE_MANAGERS.has(name)) {
      let next = null;
      for (let j = 0; j < rest.length; j++) {
        if (OPTIONS_WITH_VALUES.has(rest[j])) { j++; continue; }
        if (rest[j].startsWith("-") || rest[j] === "run" || rest[j] === "run-script") continue;
        if (/^build(:|$)/.test(rest[j])) return `${name} ${rest[j]}`;
        if (rest[j] === "exec" || rest[j] === "dlx" || rest[j] === "x") next = rest.slice(j + 1);
        break;
      }
      if (!next || !next.length) return null;
      words = next; continue;
    }
    return null;
  }
  return null;
}

function refuse(why, program, args) {
  const error = new Error(`class A refuses a compile/emit launch (${why}): ${[program, ...args].join(" ").slice(0, 300)}. Class A runs before any compile; this test row belongs after class A.`);
  error.code = REFUSAL_CODE;
  try { process.stderr.write(`rightkit-gate: ${error.message}\n`); } catch { /* stderr closed */ }
  throw error;
}

function check(program, args, options) {
  const argv = Array.isArray(args) ? args : [];
  if (options?.shell) { for (const cmd of shellWords([program, ...argv].join(" "))) { const why = compileLaunch(cmd[0], cmd.slice(1)); if (why) refuse(why, program, argv); } return; }
  const why = compileLaunch(program, argv);
  if (why) refuse(why, program, argv);
}

/** `env` with the guard switched on and loaded (NODE_OPTIONS appended, never replaced). */
export function withGuard(env) {
  const current = String(env.NODE_OPTIONS ?? "");
  return { ...env, RIGHTKIT_CLASS_A_LAUNCH_GUARD: "1", NODE_OPTIONS: current.includes(LAUNCH_GUARD_IMPORT) ? current : `${current} ${LAUNCH_GUARD_IMPORT}`.trim() };
}

export function installLaunchGuard() {
  if (globalThis[INSTALLED]) return;
  globalThis[INSTALLED] = true;
  const o = { ...childProcess };
  const split = (args) => (Array.isArray(args[1]) ? [args[0], args[1], args[2]] : [args[0], [], args[1]]);
  // A child handed a replacement environment keeps the guard: its NODE_OPTIONS (and the switch) are restored into that env.
  // The caller's options object is never modified: the launch gets a copy carrying the restored env.
  const keep = (a) => a.map((x) => (x && typeof x === "object" && !Array.isArray(x) && x.env && typeof x.env === "object" ? { ...x, env: withGuard(x.env) } : x));
  childProcess.spawn = function (...a) { const [p, v, opt] = split(a); check(p, v, opt); return o.spawn.apply(this, keep(a)); };
  childProcess.spawnSync = function (...a) { const [p, v, opt] = split(a); check(p, v, opt); return o.spawnSync.apply(this, keep(a)); };
  childProcess.execFile = function (...a) { const [p, v, opt] = split(a); check(p, v, opt); return o.execFile.apply(this, keep(a)); };
  childProcess.execFileSync = function (...a) { const [p, v, opt] = split(a); check(p, v, opt); return o.execFileSync.apply(this, keep(a)); };
  childProcess.exec = function (cmd, ...a) { for (const c of shellWords(cmd)) { const why = compileLaunch(c[0], c.slice(1)); if (why) refuse(why, cmd, []); } return o.exec.call(this, cmd, ...keep(a)); };
  childProcess.execSync = function (cmd, ...a) { for (const c of shellWords(cmd)) { const why = compileLaunch(c[0], c.slice(1)); if (why) refuse(why, cmd, []); } return o.execSync.call(this, cmd, ...keep(a)); };
  childProcess.fork = function (modulePath, ...a) { const v = Array.isArray(a[0]) ? a[0] : []; check(modulePath, v, {}); return o.fork.call(this, modulePath, ...keep(a)); };
  syncBuiltinESMExports();
}

if (process.env.RIGHTKIT_CLASS_A_LAUNCH_GUARD === "1") installLaunchGuard();
