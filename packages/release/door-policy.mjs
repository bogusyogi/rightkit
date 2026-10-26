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
const SHELLS = new Set(["bash", "sh", "zsh", "dash", "fish", "pwsh", "powershell", "cmd"]);
// cargo's global options that take a value (`cargo -C dir build`, `cargo --config k=v test`): the value is not the subcommand.
const CARGO_VALUE_OPTIONS = new Set(["-C", "--config", "--color", "-Z", "--manifest-path"]);
const WRAPPERS = new Set(["env", "command", "exec", "time", "nice", "nohup", "sudo", "timeout"]);
// Shell reserved words and grouping that can precede a command in the same simple command (`if true; then cargo build; fi`,
// `{ cargo build; }`, `f() { cargo build; }`, `! cargo test`): they are skipped, never taken for the command.
const KEYWORDS = new Set(["if", "then", "elif", "else", "fi", "do", "done", "while", "until", "{", "}", "!", "function"]);
// Adrian, 2026-10-26: every native and frontend build tool an app can be built with, not only cargo. xcodebuild's actions, swift's
// compiling subcommands, Gradle tasks, the frontend bundlers, and a package script whose name says it builds.
const XCODE_ACTIONS = new Set(["build", "archive", "test", "build-for-testing", "test-without-building", "analyze", "install", "installsrc", "clean"]);
const XCODE_READONLY = new Set(["-version", "-list", "-showsdks", "-showBuildSettings", "-showdestinations", "-showTestPlans", "-help", "-usage", "-checkFirstLaunchStatus", "-exportLocalizations"]);
const SWIFT_COMPILES = new Set(["build", "test", "run"]);
const GRADLE_READONLY = new Set(["--version", "-v", "-version", "--help", "-h", "-?", "help", "tasks", "dependencies", "projects", "properties", "--stop", "--status"]);
const FRONTEND_BUNDLERS = new Set(["webpack", "rollup", "esbuild", "tsup", "parcel"]);
// A package script whose name says it builds (`pnpm build`, `pnpm run build:mac`, `npm run tauri`): the apps have no scripts, and a
// script that builds would be a second door even if they did.
const SCRIPT_BUILDS = /^(build|bundle|package|dist|compile|release|tauri|android|ios|xcode|gradle)(:|$)/;
const DOOR_TOOLS = new Set(["cargo", "tauri", "right-release", "xcodebuild", "swift", "swiftc", "gradle", "gradlew", "vite", ...FRONTEND_BUNDLERS]);

/** Splits a shell command into simple commands (on ; && || | and newlines), each an argv of words. Quotes are respected. */
export function simpleCommands(command) {
  return pipedCommands(command).map((c) => c.argv);
}

/**
 * simpleCommands, each with `piped` (the argv whose output it reads through a single `|`, or null), `pipesOut` (its output goes
 * through `|`), `after` (the separator before it: "start", ";", "&&", "||", "|", "&") and `scope` (the nesting of subshells and
 * if/while/until/for/case/{ } blocks it stands in). An assignment reaches a later command for certain only in the same scope.
 */
function pipedCommands(command) {
  const out = []; let words = []; let word = ""; let quote = null; let has = false; let pipe = false; let sep = "start";
  const stack = []; let next = 0;
  const OPEN = { if: "fi", while: "done", until: "done", for: "done", case: "esac", "{": "}" };
  const flushWord = () => { if (has) { words.push(word); word = ""; has = false; } };
  const flushCmd = (nextSep = ";") => {
    flushWord();
    if (words.length) {
      // Block words change the scope: a closing word belongs to the inner scope it closes, an opening word to the one it opens.
      let w = 0;
      while (w < words.length && ["fi", "done", "esac", "}"].includes(words[w]) && stack.length) { stack.pop(); w++; }
      const scope = stack.map((b) => b.id).join("/");
      if (pipe && out.length) out[out.length - 1].pipesOut = true;
      out.push({ argv: words, piped: pipe && out.length ? out[out.length - 1].argv : null, pipesOut: false, after: sep, scope });
      for (; w < words.length; w++) if (OPEN[words[w]] && (w === 0 || ["then", "do", "else", "elif", "{", "!", "time"].includes(words[w - 1]) || OPEN[words[w - 1]])) stack.push({ id: next++, kind: words[w] });
      for (const x of words) if (["fi", "done", "esac", "}"].includes(x) && x !== words[0] && stack.length) stack.pop();
    }
    words = []; pipe = nextSep === "|"; sep = nextSep;
  };
  for (let i = 0; i < command.length; i++) {
    const ch = command[i];
    if (quote) { if (ch === quote) quote = null; else if (ch === "\\" && quote === '"' && i + 1 < command.length) { word += command[++i]; } else word += ch; has = true; continue; }
    if (ch === "'" || ch === '"') { quote = ch; has = true; continue; }
    if (ch === "\\" && i + 1 < command.length) { word += command[++i]; has = true; continue; }
    if (ch === "|" && command[i + 1] === "|") { flushCmd("||"); i++; continue; }
    if (ch === "&" && command[i + 1] === "&") { flushCmd("&&"); i++; continue; }
    if (ch === "|") { flushCmd("|"); continue; }
    if (ch === "(") { flushCmd(";"); stack.push({ id: next++, kind: "(" }); continue; }
    // In a case block, `)` ends a pattern; otherwise it closes a subshell.
    if (ch === ")") { const top = stack[stack.length - 1]; flushCmd(";"); if (top && top.kind !== "case" && stack.length) stack.pop(); continue; }
    if (ch === ";" || ch === "\n" || ch === "&") { flushCmd(ch === "&" ? "&" : ";"); continue; }
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
    while (a.length && KEYWORDS.has(a[0])) a = a.slice(1);
    while (a.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(a[0])) a = a.slice(1);
    if (a.length && KEYWORDS.has(a[0])) continue;
    if (a.length && WRAPPERS.has(base(a[0]))) { const w = base(a[0]); a = a.slice(1); while (a.length && a[0].startsWith("-")) a = a.slice(a[0] === "-u" || a[0] === "-n" ? 2 : 1); if (w === "timeout" && a.length && /^\d/.test(a[0])) a = a.slice(1); continue; }
    return a;
  }
}

/** The violation a single argv is, or null. */
/** argv without its redirections (`>/dev/null`, `2>&1`, `> out.txt`, `<in`): they are not arguments of the command. */
function withoutRedirects(argv) {
  const out = [];
  for (let i = 0; i < argv.length; i++) {
    if (/^(\d*|&)(>>?|<)(&\d*)?$/.test(argv[i])) { i++; continue; }
    if (/^(\d*|&)(>>?|<)/.test(argv[i])) continue;
    out.push(argv[i]);
  }
  return out;
}

function argvViolation(argv, depth = 0, stdin = null) {
  let a = stripWrappers(withoutRedirects(argv));
  if (!a.length) return null;
  let tool = base(a[0]);
  // A command string handed to another shell (`bash -c "cargo build"`, `eval "..."`) is judged as the command it is.
  if (depth < 4 && (SHELLS.has(tool.toLowerCase()) || tool === "eval")) {
    const flag = a.findIndex((w, i) => i > 0 && /^(-c|\/c|-Command|-command)$/.test(w));
    const script = tool === "eval" ? a.slice(1).join(" ") : flag > 0 ? a.slice(flag + 1).join(" ") : null;
    return script ? doorViolation(script, depth + 1) : null;
  }
  if (tool === "xargs" && a.length > 1) {
    const inner = a.slice(1).filter((w) => !w.startsWith("-"));
    // xargs appends what it reads to the command. Read from a literal `echo`/`printf`, the words are known and judged with it;
    // read from anything else, a door tool, a package runner or node is denied, since the text cannot say what it will run.
    const echoed = stdin && ["echo", "printf"].includes(base(stdin[0])) ? stdin.slice(base(stdin[0]) === "printf" ? 2 : 1).filter((w) => !/^-[neE]+$/.test(w)).flatMap((w) => w.split(/\s+/).filter(Boolean)) : null;
    if (echoed) return argvViolation([...inner, ...echoed], depth + 1);
    if (DOOR_TOOLS.has(base(inner[0])) || RUNNERS.has(base(inner[0])) || base(inner[0]) === "node") { const v = argvViolation(inner, depth + 1); return v ?? `${base(inner[0])} (arguments from xargs)`; }
    return argvViolation(inner, depth + 1);
  }
  // pnpm/npx/yarn [exec|dlx] <tool> ...: judge the tool it runs.
  if (RUNNERS.has(tool)) {
    let rest = a.slice(1).filter((w, i, all) => !(w.startsWith("-") || (i > 0 && ["--filter", "-F", "--dir", "-C", "--prefix"].includes(all[i - 1]))));
    if (["exec", "dlx", "x"].includes(rest[0])) rest = rest.slice(1);
    const script = rest[0] === "run" || rest[0] === "run-script";
    if (script) rest = rest.slice(1);
    if (!rest.length) return null;
    // A script whose name says it builds is denied as the runner's script (`pnpm build`, `npm run tauri`); any other script name is
    // not a tool (the apps have none). Without `run`, the word is judged as the tool it names (`pnpm tauri build`, `pnpm exec vite build`).
    if (!script && DOOR_TOOLS.has(base(rest[0]))) { a = rest; tool = base(a[0]); }
    else if (SCRIPT_BUILDS.test(rest[0])) return `${tool} ${rest[0]}`;
    else if (script) return null;
    else { a = rest; tool = base(a[0]); }
  }
  if (tool === "rightkit-gate" || tool === "rustright") return null;
  if (tool === "tauri" && TAURI_BUILDS.has(a[1])) return `tauri ${a[1]}`;
  // Apple: xcrun wraps the tool it names; xcodebuild builds by default (no action word is `build`); swift build|test|run and swiftc compile.
  if (tool === "xcrun" && a.length > 1) return argvViolation(a.slice(1), depth + 1, stdin);
  if (tool === "xcodebuild") {
    const words = a.slice(1);
    const action = words.find((w) => XCODE_ACTIONS.has(w));
    if (action) return `xcodebuild ${action}`;
    return words.some((w) => XCODE_READONLY.has(w)) ? null : "xcodebuild build";
  }
  if (tool === "swift" && SWIFT_COMPILES.has(a[1])) return `swift ${a[1]}`;
  if (tool === "swiftc") return "swiftc";
  // Android: a Gradle task that is not one of the read-only reports.
  if (tool === "gradle" || tool === "gradlew") {
    const task = a.slice(1).find((w) => !w.startsWith("-"));
    if (task && !GRADLE_READONLY.has(task)) return `gradle ${task}`;
    if (!task && !a.slice(1).some((w) => GRADLE_READONLY.has(w))) return null; // bare gradle prints help
    return null;
  }
  // Frontend: a production bundle is the compile stage's frontend build.
  if (tool === "vite" && a[1] === "build") return "vite build";
  if (FRONTEND_BUNDLERS.has(tool)) return tool;
  if (tool === "cargo") {
    let sub = null;
    for (let i = 1; i < a.length; i++) {
      if (CARGO_VALUE_OPTIONS.has(a[i])) { i++; continue; }
      if (!a[i].startsWith("-") && !a[i].startsWith("+")) { sub = a[i]; break; }
    }
    if (sub === "tauri" && TAURI_BUILDS.has(a[a.indexOf("tauri") + 1])) return `cargo tauri ${a[a.indexOf("tauri") + 1]}`;
    if (sub && CARGO_COMPILES.has(sub) && sub !== "tauri") return `cargo ${sub}`;
    return null;
  }
  const releaseArgs = tool === "right-release" ? a.slice(1) : tool === "node" && a[1] && /right-release(\.mjs)?$/.test(a[1]) ? a.slice(2) : null;
  if (releaseArgs) {
    // A flag first (`right-release --platform mac`) is `build`; `cargo-target` and `dmg build` compile too.
    if (releaseArgs[0]?.startsWith("-") && !["-h", "--help", "-v", "--version"].includes(releaseArgs[0])) return "right-release build";
    if (RELEASE_COMMANDS.has(releaseArgs[0])) return `right-release ${releaseArgs[0]}`;
    if (releaseArgs[0] === "cargo-target") return "right-release cargo-target";
    if (["dmg", "addon"].includes(releaseArgs[0]) && releaseArgs[1] === "build") return `right-release ${releaseArgs[0]} build`;
  }
  // The build engine called directly.
  if (tool === "node" && a[1] && /(^|[\/])(build-release|release|upload-release|publish-update)\.mjs$/.test(a[1])) return "right-release build";
  return null;
}

/** The bodies of `$(...)` and backtick substitutions in `command`, outside single quotes (they run inside double quotes too). */
export function substitutions(command) {
  return splitSubstitutions(command).bodies;
}

/** { bodies, rest }: the substitution bodies, and `command` with each substitution replaced by a placeholder word. */
function splitSubstitutions(command) {
  const out = []; let quote = null; let rest = ""; let from = 0;
  const cut = (start, end, body) => { out.push(body); rest += `${command.slice(from, start)}_SUBST_`; from = end; };
  for (let i = 0; i < command.length; i++) {
    const ch = command[i];
    if (quote === "'") { if (ch === "'") quote = null; continue; }
    if (ch === "\\") { i++; continue; }
    if (ch === "'" && quote !== '"') { quote = "'"; continue; }
    if (ch === '"') { quote = quote === '"' ? null : '"'; continue; }
    if (ch === "`") { const end = command.indexOf("`", i + 1); if (end < 0) { cut(i, command.length, command.slice(i + 1)); break; } cut(i, end + 1, command.slice(i + 1, end)); i = end; continue; }
    if (ch === "$" && command[i + 1] === "(" && command[i + 2] !== "(") {
      let depth = 0; let j = i + 1;
      // A `case` arm's pattern ends in an unmatched `)` (`$(case x in x) cargo build;; esac)`): it does not close the substitution.
      const openCases = (to) => (command.slice(i + 2, to).match(/\bcase\b/g) ?? []).length - (command.slice(i + 2, to).match(/\besac\b/g) ?? []).length;
      for (; j < command.length; j++) { if (command[j] === "(") depth++; else if (command[j] === ")" && depth === 1 && openCases(j) > 0) continue; else if (command[j] === ")" && --depth === 0) break; }
      cut(i, j + 1, command.slice(i + 2, j)); i = j;
    }
  }
  return { bodies: out, rest: rest + command.slice(from) };
}

/** The first way `command` builds outside the door, or null. */
export function doorViolation(command, depth = 0) {
  if (typeof command !== "string" || !command.trim()) return null;
  const { bodies, rest } = splitSubstitutions(command);
  if (depth < 4) for (const inner of bodies) { const v = doorViolation(inner, depth + 1); if (v) return v; }
  // Variables set in the command (`T=cargo; $T build`, `export T=cargo`) are substituted where they stand in command position; a
  // command word that is a variable the command does not set cannot be read from the text, so it is denied rather than guessed.
  const vars = {};
  const commands = pipedCommands(rest);
  for (const [index, { argv, piped, pipesOut, after, scope }] of commands.entries()) {
    let a = argv;
    // `time` is a reserved word in bash too: `time T=cargo` assigns.
    while (a.length && (KEYWORDS.has(a[0]) || a[0] === "time" || (a[0] === "-p" && argv[0] === "time"))) a = a.slice(1);
    while (a.length && ["export", "declare", "local", "readonly", "typeset"].includes(a[0])) a = a.slice(1).filter((w) => !w.startsWith("-"));
    const assigns = []; while (a.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(a[0])) { assigns.push(a[0]); a = a.slice(1); }
    // Certain only when it runs in this shell unconditionally: not a pipeline element, not after && or ||, not backgrounded.
    // (`true && T=x` and `false || T=x` always run it; any other condition cannot be decided from the text.)
    const prev = commands[index - 1]?.argv;
    const decided = prev?.length === 1 && ((after === "&&" && prev[0] === "true") || (after === "||" && prev[0] === "false"));
    const sure = !piped && !pipesOut && (after === "start" || after === ";" || decided);
    if (!a.length) { for (const w of assigns) { const k = w.slice(0, w.indexOf("=")); vars[k] = { value: w.slice(k.length + 1), sure, scope }; } continue; }
    const head = stripWrappers(a);
    const ref = /^\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))$/.exec(head[0] ?? "");
    if (ref) {
      const name = ref[1] ?? ref[2];
      if (!(name in vars)) return `\$${name} (a command the text does not name)`;
      // An assignment that may not have reached this command (a pipeline element, a subshell, another block, after && or ||) may
      // leave the variable empty, and then its arguments are the command: both readings are judged.
      const set = vars[name];
      if (!set.sure || !(scope === set.scope || scope.startsWith(`${set.scope}/`) || set.scope === "")) {
        const empty = argvViolation(head.slice(1), 0, piped ? stripWrappers(withoutRedirects(piped)) : null);
        if (empty) return empty;
      }
      a = [...set.value.split(/\s+/).filter(Boolean), ...head.slice(1)];
    }
    const v = argvViolation(a, 0, piped ? stripWrappers(withoutRedirects(piped)) : null);
    if (v) return v;
  }
  return null;
}

export function doorDenial(violation) {
  const mobile = /^(cargo )?tauri (android|ios)$|^xcodebuild (archive|install)|^gradle (assemble|bundle|build|publish|install)/.test(violation) ? (/ios|xcodebuild/.test(violation) ? "ios" : "android") : null;
  const stage = /^right-release (dev|test)$/.test(violation) ? violation.slice("right-release ".length) : /^(cargo )?tauri dev$/.test(violation) ? "dev"
    : /^right-release (upload|publish|publish-update|github)$/.test(violation) ? "release" : /^right-release qa$/.test(violation) ? "qualify"
    : /^right-release /.test(violation) || /^(cargo )?tauri (build|bundle|android|ios)$/.test(violation) || /^xcodebuild (archive|install)/.test(violation) || /^gradle /.test(violation) ? "package"
    : "compile";
  const platform = mobile ?? "mac|win";
  const tail = stage === "package" || stage === "qualify" ? ` --platform ${platform}` : stage === "release" ? ` --platform ${platform}${mobile ? "" : " --tier patch|update"}` : stage === "dev" || stage === "test" ? " -- <arguments>" : "";
  return `[HOOK:one-door:block]\nWhy: \`${violation}\` builds outside the one door. Every check, compile, test, package and release goes through rightkit-gate, class A first.\nRequired: rightkit-gate --to ${stage}${tail} (or rustright cargo ... on a broker host, which runs class A first).`;
}

/** Claude Code PreToolUse decision for one hook input. */
export function claudeDoorDecision(input) {
  const command = input?.tool_input?.command ?? input?.tool_input?.cmd ?? input?.command ?? null;
  const violation = doorViolation(typeof command === "string" ? command : "");
  return violation
    ? { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: doorDenial(violation) } }
    : {};
}
