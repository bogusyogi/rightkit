// The one-door policy for agents (Adrian, 2026-10-25): one way to build, through rightkit-gate. An agent's shell command that builds,
// checks, tests, packages or releases an app any other way is denied with the door to use instead. Deterministic and offline: it reads
// only the command text. `rustright cargo ...` is allowed (the broker admits a compile only after class A); `rightkit-gate ...` is the
// door. The same policy runs on every host: through the repository's .claude/settings.json (rightkit-gate install-hook), so a cloud
// session without the broker is held to it too.
import path from "node:path";

const CARGO_COMPILES = new Set(["build", "b", "check", "c", "clippy", "test", "t", "run", "r", "bench", "doc", "d", "rustc", "install", "tauri"]);
const TAURI_BUILDS = new Set(["build", "dev", "bundle", "android", "ios"]);
// right-release commands that build, package, sign, qualify or ship (CHECKED_COMMANDS "commit" in app-check.mjs).
const RELEASE_COMMANDS = new Set(["dev", "test", "stage-sidecar", "release", "build", "package", "qa", "upload", "publish", "publish-update", "github", "sign-windows", "sign-updater", "create-mac-updater", "web-build", "swift-package", "recipe-build", "sign-candidate"]);
const RUNNERS = new Set(["pnpm", "npm", "npx", "yarn", "bunx", "pnpx"]);
const WRAPPERS = new Set(["env", "command", "exec", "time", "nice", "nohup", "sudo", "timeout"]);

/** Splits a shell command into simple commands (on ; && || | and newlines), each an argv of words. Quotes are respected. */
export function simpleCommands(command) {
  const out = []; let words = []; let word = ""; let quote = null; let has = false;
  const flushWord = () => { if (has) { words.push(word); word = ""; has = false; } };
  const flushCmd = () => { flushWord(); if (words.length) out.push(words); words = []; };
  for (let i = 0; i < command.length; i++) {
    const ch = command[i];
    if (quote) { if (ch === quote) quote = null; else if (ch === "\\" && quote === '"' && i + 1 < command.length) { word += command[++i]; } else word += ch; has = true; continue; }
    if (ch === "'" || ch === '"') { quote = ch; has = true; continue; }
    if (ch === "\\" && i + 1 < command.length) { word += command[++i]; has = true; continue; }
    if (ch === ";" || ch === "\n" || ch === "|" || ch === "&" || ch === "(" || ch === ")") { flushCmd(); continue; }
    if (/\s/.test(ch)) { flushWord(); continue; }
    word += ch; has = true;
  }
  flushCmd();
  return out;
}

const base = (word) => path.basename(String(word ?? "")).replace(/\.(exe|cmd|bat|mjs|js)$/i, "");

function stripWrappers(argv) {
  let a = [...argv];
  for (;;) {
    while (a.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(a[0])) a = a.slice(1);
    if (a.length && WRAPPERS.has(base(a[0]))) { a = a.slice(1); while (a.length && a[0].startsWith("-")) a = a.slice(a[0] === "-u" || a[0] === "-n" ? 2 : 1); if (base(argv[0]) === "timeout" && a.length && /^\d/.test(a[0])) a = a.slice(1); continue; }
    return a;
  }
}

/** The violation a single argv is, or null. */
function argvViolation(argv) {
  let a = stripWrappers(argv);
  if (!a.length) return null;
  let tool = base(a[0]);
  // pnpm/npx/yarn [exec|dlx] <tool> ...: judge the tool it runs.
  if (RUNNERS.has(tool)) {
    let rest = a.slice(1).filter((w, i, all) => !(w.startsWith("-") || (i > 0 && ["--filter", "-F", "--dir", "-C", "--prefix"].includes(all[i - 1]))));
    if (["exec", "dlx", "x"].includes(rest[0])) rest = rest.slice(1);
    if (rest[0] === "run" || rest[0] === "run-script") return null; // the apps have no scripts; a script name is not a tool
    if (!rest.length) return null;
    a = rest; tool = base(a[0]);
  }
  if (tool === "rightkit-gate" || tool === "rustright") return null;
  if (tool === "tauri" && TAURI_BUILDS.has(a[1])) return `tauri ${a[1]}`;
  if (tool === "cargo") {
    const sub = a.slice(1).find((w) => !w.startsWith("-") && !w.startsWith("+"));
    if (sub === "tauri" && TAURI_BUILDS.has(a[a.indexOf("tauri") + 1])) return `cargo tauri ${a[a.indexOf("tauri") + 1]}`;
    if (sub && CARGO_COMPILES.has(sub) && sub !== "tauri") return `cargo ${sub}`;
    return null;
  }
  if (tool === "right-release" && RELEASE_COMMANDS.has(a[1])) return `right-release ${a[1]}`;
  if (tool === "node" && a[1] && /right-release(\.mjs)?$/.test(a[1]) && RELEASE_COMMANDS.has(a[2])) return `right-release ${a[2]}`;
  return null;
}

/** The first way `command` builds outside the door, or null. */
export function doorViolation(command) {
  if (typeof command !== "string" || !command.trim()) return null;
  for (const argv of simpleCommands(command)) {
    const v = argvViolation(argv);
    if (v) return v;
  }
  return null;
}

export function doorDenial(violation) {
  const stage = /^right-release (dev|test)$/.test(violation) ? violation.slice("right-release ".length) : /^(cargo )?tauri dev$/.test(violation) ? "dev"
    : /^right-release (upload|publish|publish-update|github)$/.test(violation) ? "release" : /^right-release qa$/.test(violation) ? "qualify" : /^right-release /.test(violation) || /^(cargo )?tauri (build|bundle|android|ios)$/.test(violation) ? "package" : "compile";
  return `[HOOK:one-door:block]\nWhy: \`${violation}\` builds outside the one door. Every check, compile, test, package and release goes through rightkit-gate, class A first.\nRequired: rightkit-gate --to ${stage}${stage === "package" || stage === "qualify" || stage === "release" ? " --platform mac|win" : ""}${stage === "dev" || stage === "test" ? " -- <arguments>" : ""}${stage === "release" ? " --tier patch|update" : ""} (on a machine with the RustRight broker, \`rustright cargo ...\` is also admitted: the broker runs class A first).`;
}

/** Claude Code PreToolUse decision for one hook input. */
export function claudeDoorDecision(input) {
  const command = input?.tool_input?.command ?? input?.tool_input?.cmd ?? input?.command ?? null;
  const violation = doorViolation(typeof command === "string" ? command : "");
  return violation
    ? { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: doorDenial(violation) } }
    : {};
}
