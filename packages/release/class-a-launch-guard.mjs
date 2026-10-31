// Class A launch guard: class A runs before any compile, so a class A test row may not launch a compiler or an emitting build.
//
// The gate runs every class A test row with NODE_OPTIONS carrying `--import=<this file>`. In every Node process of the row (the
// test runner, pnpm, workers, any Node child they launch, including one given a replacement environment) the child_process launchers are wrapped, and a launch is
// refused (it throws, so the row fails) when the program, or a command line it hands to a shell, is a compile or an emitting
// build: cargo, rustc, rustright, rightkit (broker client), tauri / cargo-tauri, tsc without --noEmit, tsup, esbuild, swc, rollup,
// webpack, `vite build`, `next build`, or a package-manager run of a build / build:* script.
//
// Shell command text (exec, execSync, `shell: true`, `sh -c <text>`, `cmd /c <text>`, ...) is read by a small shell parser BEFORE the
// shell starts, and it fails closed: text the guard cannot read to the end is refused, never passed on. Refused before launch:
// command substitution `$( )` and backticks (also inside double quotes), process substitution, parameter expansions that evaluate
// text (`${!x}`, `${x@P}`, zsh `${(e)x}`), ANSI-C quoting `$'..'`, unquoted parentheses and braces (subshells, functions, groups,
// brace expansion), shell control forms (if/for/while/until/case/select/function/`!`), `eval`/`source`/`.`/`trap`/`alias`, programs
// that launch a program they are told (xargs, parallel, concurrently, run-s, ...), a command word built by expansion or a glob,
// and a shell that reads its commands from stdin. Newlines separate commands; backslash and quote joins are resolved before the
// program is named; leading assignments and redirections never hide the program; wrappers (env, nice, timeout, command, nohup, sudo,
// busybox, find, ...) have every following word checked as a possible program; shells nest to a bounded depth and a chain the guard
// cannot finish reading is refused. Ordinary noncompiling commands (echo, test, node, git, ... with plain arguments, including
// `$VAR` arguments and quoted text that merely names a compiler) are unchanged. Prefer an argv launch (spawn/execFile with an
// array) for Class A rows: it never reaches the shell parser.
//
// Scope, exactly: launches from Node processes of the row. Not covered: a compiler API called inside a process
// (typescript.createProgram().emit(), vite.build() / the rollup JS API), launches by a non-Node program that does not load
// this hook (rightkit#1240: a shell script file, `python -c`, `perl -e`, `awk system()`), and the contents of a script file a shell
// runs. esbuild's JS API is covered: it launches the esbuild service binary.
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
const SHELLS = new Set(["sh", "bash", "zsh", "dash", "ash", "ksh", "mksh", "fish", "csh", "tcsh", "cmd", "powershell", "pwsh"]);
// Programs that run another program they are given: every later word is checked as a possible program (options and their values cannot
// be told apart from the program without each tool's grammar, so the check is deliberately wide).
const SCAN_WRAPPERS = new Set([
  "env", "exec", "time", "nice", "npx", "pnpx", "command", "builtin", "nohup", "timeout", "setsid", "stdbuf", "ionice", "taskset", "chrt",
  "flock", "sudo", "doas", "busybox", "xvfb-run", "unbuffer", "watch", "strace", "ltrace", "cross-env", "dotenv", "find", "call", "start", "arch", "script",
]);
// Programs whose job is to launch commands from text or arguments the guard cannot read: refused outright.
const LAUNCHER_DYNAMIC = new Set(["xargs", "parallel", "concurrently", "npm-run-all", "run-s", "run-p", "iex", "invoke-expression", "invoke-command"]);
// Shell builtins that run text (shell text only).
const TEXT_DYNAMIC = new Set(["eval", "source", ".", "trap", "alias", "coproc"]);
const CONTROL_WORDS = new Set(["if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "select", "function", "{", "}", "!"]);
// Options that take a value, per package manager: pnpm's `-w` is `--workspace-root` (a flag), npm's `-w` is `--workspace <name>`.
const OPTIONS_WITH_VALUES = Object.freeze({
  pnpm: new Set(["--filter", "-F", "--dir", "-C"]),
  npm: new Set(["--workspace", "-w", "--prefix"]),
  yarn: new Set(["--cwd"]),
  bun: new Set(["--cwd", "--filter"]),
});
const MAX_DEPTH = 12;
const DYN = "\u0000"; // prefixes a shell-text word whose value is decided by expansion or a glob (never present in a real argv)
const ASSIGN = /^[A-Za-z_][A-Za-z0-9_]*\+?=/;
const REDIRECT = /^(?:\d*|&)(?:<<<|<<-?|<>|<&|>&|>>|>\||<|>)/;
const REDIRECT_ONLY = /^(?:\d*|&)(?:<<<|<<-?|<>|<&|>&|>>|>\||<|>)$/;

const base = (p) => path.basename(String(p ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat|ps1|js|mjs|cjs)$/i, "").toLowerCase();
const isDyn = (w) => typeof w === "string" && w.startsWith(DYN);
const plain = (w) => (isDyn(w) ? w.slice(1) : w);

/**
 * Reads shell command text into commands and words (quotes and escapes resolved), or reports why it cannot be read safely.
 * dialect: "sh" (POSIX-like shells), "cmd" (cmd.exe: backslash is a path character, only double quotes quote), "pwsh" (backtick escapes).
 * A word decided by an expansion or an unquoted glob is returned prefixed with DYN. Returns { commands, meta, hazard }: `hazard` is
 * a string when the text uses syntax the guard refuses, and `commands` is then only what was read before it.
 */
function parseShell(text, dialect = "sh") {
  const s = String(text);
  const posix = dialect === "sh";
  const commands = []; const meta = [];
  let words = []; let word = ""; let started = false; let dyn = false; let quote = null;
  let cur = { piped: false, heredoc: false };
  const end = () => {
    if (word || started) {
      if (dyn && !ASSIGN.test(word) && !REDIRECT.test(word) && word !== "[") word = DYN + word;
      if (word) words.push(word);
    }
    word = ""; started = false; dyn = false;
  };
  const commit = () => { end(); if (words.length) { commands.push(words); meta.push(cur); cur = { piped: false, heredoc: false }; } words = []; };
  const stop = (hazard) => { return { commands, meta, hazard }; };
  const expands = (c) => c !== undefined && /[A-Za-z_0-9{@*#?$!-]/.test(c);
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (quote === "'") { if (c === "'") quote = null; else word += c; continue; }
    if (quote === "\"") {
      if (c === "\"") { quote = null; continue; }
      if (c === "\\" && posix) {
        const n = s[i + 1];
        if (n !== undefined && "$`\"\\\n".includes(n)) { if (n !== "\n") word += n; i++; } else word += "\\";
        continue;
      }
      if (c === "`" && posix) return stop("backtick command substitution");
      if (c === "%" && dialect === "cmd") dyn = true;
      if (c === "$") {
        if (s[i + 1] === "(") return stop("command substitution $( )");
        if (s[i + 1] === "{") {
          const close = s.indexOf("}", i + 2);
          if (close < 0) return stop("unterminated ${ }");
          const inner = s.slice(i + 2, close);
          if (/^[(!]/.test(inner) || inner.includes("@") || inner.includes("$(") || inner.includes("`")) return stop("parameter expansion that evaluates text");
          word += s.slice(i, close + 1); dyn = true; i = close; continue;
        }
        if (expands(s[i + 1])) dyn = true;
      }
      word += c; continue;
    }
    if (c === "'" && dialect !== "cmd") { quote = "'"; started = true; continue; }
    if (c === "\"") { quote = "\""; started = true; continue; }
    if (c === "\\" && posix) {
      const n = s[i + 1];
      if (n === undefined) { word += "\\"; continue; }
      if (n === "\n") { i++; continue; }
      word += n; started = true; i++; continue;
    }
    if (c === "`") {
      if (dialect === "pwsh") { word += s[i + 1] ?? ""; started = true; i++; continue; }
      if (dialect === "cmd") { word += c; continue; }
      return stop("backtick command substitution");
    }
    if (c === "$") {
      if (s[i + 1] === "(") return stop("command substitution $( )");
      if (s[i + 1] === "{") {
        const close = s.indexOf("}", i + 2);
        if (close < 0) return stop("unterminated ${ }");
        const inner = s.slice(i + 2, close);
        if (/^[(!]/.test(inner) || inner.includes("@") || inner.includes("$(") || inner.includes("`")) return stop("parameter expansion that evaluates text");
        word += s.slice(i, close + 1); dyn = true; i = close; continue;
      }
      if (s[i + 1] === "'" && posix) return stop("ANSI-C quoting $'...'");
      if (s[i + 1] === "\"" && posix) continue; // $"text": a locale-translated double-quoted string
      if (expands(s[i + 1])) dyn = true;
      word += c; continue;
    }
    if (c === "%" && dialect === "cmd") { dyn = true; word += c; continue; }
    if (c === "^" && dialect === "cmd") return stop("cmd caret escape");
    if (c === "(" || c === ")") return stop("unquoted parenthesis (subshell, function, process substitution or array)");
    if (c === "{") {
      if (s[i + 1] === "}") { word += "{}"; i++; continue; }
      return stop("unquoted brace (group, function body or brace expansion)");
    }
    if ((c === "*" || c === "?" || c === "[") && !word && !started && c === "[" && (s[i + 1] === " " || s[i + 1] === undefined)) { word += c; continue; }
    if (c === "*" || c === "?" || c === "[") { dyn = true; word += c; continue; }
    if (c === "=" && !word && !started && posix) { dyn = true; word += c; continue; } // zsh `=cmd` expands to the path of cmd
    if (c === "#" && !word && !started && dialect !== "cmd") { const nl = s.indexOf("\n", i); if (nl < 0) break; i = nl - 1; continue; }
    if (c === "\n") { commit(); continue; }
    if (/\s/.test(c)) { end(); continue; }
    if (c === "&" && (/[<>]$/.test(word) || s[i + 1] === ">")) { word += c; continue; }
    if (c === ";" || c === "|" || c === "&") {
      const pipe = c === "|" && s[i + 1] !== "|" && s[i - 1] !== "|";
      end(); commit(); if (pipe) cur.piped = true;
      continue;
    }
    if (c === "<" && s[i + 1] === "<") cur.heredoc = true;
    word += c;
  }
  if (quote) return stop("unterminated quote");
  commit();
  return { commands, meta, hazard: null };
}

/** A shell command line split into commands (on && || ; | & and newlines) and words, quotes and escapes resolved: enough to read what a `sh -c` text launches. */
export function shellWords(text) {
  return parseShell(text).commands.map((c) => c.map(plain));
}

const dialectOfShell = (name) => (name === "cmd" ? "cmd" : name === "powershell" || name === "pwsh" ? "pwsh" : "sh");
function dialectOfOptions(options) {
  if (typeof options?.shell === "string") return dialectOfShell(base(options.shell));
  return process.platform === "win32" ? "cmd" : "sh";
}

/** Drops leading assignments (`CC=x`) and redirections (`>out`, `2>&1`, `> file`) from a shell-text command: neither hides the program. */
function stripPrefixes(words) {
  let w = words;
  while (w.length) {
    const first = plain(w[0]);
    if (ASSIGN.test(first)) { w = w.slice(1); continue; }
    if (!isDyn(w[0]) && REDIRECT.test(w[0])) { w = REDIRECT_ONLY.test(w[0]) ? w.slice(2) : w.slice(1); continue; }
    break;
  }
  return w;
}

/** Why one command of shell text is a compile (or cannot be read safely), or null. */
function commandWhy(cmd, dialect, depth) {
  const words = stripPrefixes(cmd);
  if (!words.length) return null;
  if (isDyn(words[0])) return "shell command word is built by an expansion or a glob";
  if (CONTROL_WORDS.has(words[0])) return `shell control/function syntax (${words[0]})`;
  return launchWhy(words, { text: true, strict: true, dialect }, depth);
}

/** Why shell text is a compile or unreadable, or null. */
function shellTextWhy(text, dialect = "sh", depth = 0) {
  const parsed = parseShell(text, dialect);
  if (parsed.hazard) return `shell syntax refused before launch: ${parsed.hazard}`;
  for (const cmd of parsed.commands) { const why = commandWhy(cmd, dialect, depth); if (why) return why; }
  return null;
}

/** A shell invoked as `[name, ...args]`: reads its `-c` text, and refuses a shell that would read its commands from stdin. */
function shellLaunchWhy(name, words, ctx, depth) {
  const dialect = dialectOfShell(name);
  const rest = words.slice(1);
  const flag = dialect === "cmd" ? /^\/[ckr]$/i : dialect === "pwsh" ? /^-(c|command)$/i : /^-[A-Za-z]*c[A-Za-z]*$/;
  const at = rest.findIndex((w) => flag.test(w));
  if (dialect === "pwsh" && rest.some((w) => /^-(e|ec|enc|encodedcommand)$/i.test(w))) return `${name} -EncodedCommand (text the guard cannot read)`;
  if (at >= 0) {
    let j = at + 1;
    if (dialect === "sh") { while (rest[j] === "--") j++; }
    if (rest[j] === undefined) return null;
    if (ctx.text && rest.slice(0, j + 1).some(isDyn)) return `${name} is given options or command text built by an expansion or a glob`;
    const text = dialect === "sh" ? rest[j] : rest.slice(j).join(" ");
    return shellTextWhy(text, dialect, depth + 1);
  }
  if (ctx.text && rest.some(isDyn)) return `${name} is given arguments built by an expansion or a glob`;
  if (!ctx.strict) return null;
  // No -c text: the shell runs a script file (not readable here, out of scope) or reads commands from stdin (refused).
  const valueOptions = new Set(["-o", "-O", "+o", "+O", "--rcfile", "--init-file"]);
  let operand = false; let stdin = false;
  for (let k = 0; k < rest.length; k++) {
    const w = rest[k];
    if (dialect === "sh" && valueOptions.has(w)) { k++; continue; }
    if (REDIRECT.test(w)) { if (/^\d*</.test(w)) stdin = true; if (REDIRECT_ONLY.test(w)) k++; continue; }
    if (w === "-" || (dialect === "sh" && /^-[A-Za-z]*s[A-Za-z]*$/.test(w))) { stdin = true; continue; }
    if (dialect === "pwsh" && /^-file$/i.test(w)) { operand = true; break; }
    if (dialect === "cmd" ? w.startsWith("/") : w.startsWith("-") || w.startsWith("+")) continue;
    operand = true; break;
  }
  if (stdin || !operand) return `${name} reading its commands from standard input (text the guard cannot read)`;
  return null;
}

/** Why `words` (a program and its arguments) is a compile or an unreadable launch, or null. */
function launchWhy(words, ctx, depth) {
  if (depth > MAX_DEPTH) return "launch nests too deeply to inspect";
  let w = words.map(String);
  for (let hop = 0; hop < MAX_DEPTH && w.length; hop++) {
    if (ctx.text && isDyn(w[0])) return "a program word is built by an expansion or a glob";
    const name = base(w[0]);
    if (name === "node" && w[1] && !String(w[1]).startsWith("-")) { w = w.slice(1); continue; }
    if (SHELLS.has(name)) return shellLaunchWhy(name, w, ctx, depth);
    if (LAUNCHER_DYNAMIC.has(name)) return `${name} (runs a command the guard cannot read)`;
    if (ctx.text && (TEXT_DYNAMIC.has(name) || TEXT_DYNAMIC.has(w[0]))) return `${w[0]} (runs shell text the guard cannot read)`;
    const rest = w.slice(1);
    if (SCAN_WRAPPERS.has(name)) {
      if (name === "command" && /^-[vV]$/.test(rest[0] ?? "")) return null;
      for (let i = 0; i < rest.length; i++) {
        if (ctx.text && isDyn(rest[i]) && !ASSIGN.test(plain(rest[i]))) return `${name} is followed by a word built by an expansion or a glob`;
        const why = launchWhy(rest.slice(i), ctx, depth + 1);
        if (why) return why;
      }
      return null;
    }
    if (COMPILERS.has(name)) return name;
    if (name === "tsc") return rest.some((x) => /^--?noEmit(=true)?$/i.test(x)) ? null : "tsc (emits)";
    if (name === "vite" || name === "next") {
      if (rest.some(isDyn)) return `${name} is given a word built by an expansion or a glob`;
      return rest.find((x) => !x.startsWith("-")) === "build" ? `${name} build` : null;
    }
    if (PACKAGE_MANAGERS.has(name)) {
      let next = null;
      for (let j = 0; j < rest.length; j++) {
        if (isDyn(rest[j])) return `${name} is given a script or command built by an expansion or a glob`;
        if (OPTIONS_WITH_VALUES[name].has(rest[j])) { j++; continue; }
        if (rest[j].startsWith("-") || rest[j] === "run" || rest[j] === "run-script") continue;
        if (/^build(:|$)/.test(rest[j])) return `${name} ${rest[j]}`;
        if (rest[j] === "exec" || rest[j] === "dlx" || rest[j] === "x") next = rest.slice(j + 1);
        break;
      }
      if (!next || !next.length) return null;
      w = next; continue;
    }
    return null;
  }
  return w.length ? "launch chain too long to inspect" : null;
}

/** Why `[program, ...args]` is a compile or an emitting build, or null. A shell given a `-c` text is read by the shell parser (fail closed). */
export function compileLaunch(program, args = []) {
  return launchWhy([String(program), ...args.map(String)], { text: false, strict: false }, 0);
}

function refuse(why, program, args) {
  const error = new Error(`class A refuses a compile/emit launch (${why}): ${[program, ...args].join(" ").slice(0, 300)}. Class A runs before any compile; this test row belongs after class A.`);
  error.code = REFUSAL_CODE;
  try { process.stderr.write(`rightkit-gate: ${error.message}\n`); } catch { /* stderr closed */ }
  throw error;
}

function check(program, args, options) {
  const argv = Array.isArray(args) ? args : [];
  const why = options?.shell
    ? shellTextWhy([program, ...argv].join(" "), dialectOfOptions(options), 0)
    : launchWhy([String(program), ...argv.map(String)], { text: false, strict: true }, 0);
  if (why) refuse(why, program, argv);
}

function checkShellText(cmd, a) {
  const options = a.find((x) => x && typeof x === "object");
  const why = shellTextWhy(String(cmd), dialectOfOptions(options ?? {}), 0);
  if (why) refuse(why, cmd, []);
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
  childProcess.exec = function (cmd, ...a) { checkShellText(cmd, a); return o.exec.call(this, cmd, ...keep(a)); };
  childProcess.execSync = function (cmd, ...a) { checkShellText(cmd, a); return o.execSync.call(this, cmd, ...keep(a)); };
  childProcess.fork = function (modulePath, ...a) { const v = Array.isArray(a[0]) ? a[0] : []; check(modulePath, v, {}); return o.fork.call(this, modulePath, ...keep(a)); };
  syncBuiltinESMExports();
}

if (process.env.RIGHTKIT_CLASS_A_LAUNCH_GUARD === "1") installLaunchGuard();
