// The app check: step 1 of docs/PIPELINE-ARCHITECTURE.md for any repository that uses RightKit, shipped in @rightkit/release so an
// app runs it without a RightKit checkout (`rightkit-gate check`, `right-release check`, the pre-push hook, and the start of every
// right-release command that builds or ships; see CHECKED_COMMANDS). RightKit's own scripts/preflight.mjs imports these rules and
// adds the ones that police RightKit itself.
//
// It reads the committed files of one commit (or, for a compile, the tree on disk: `--worktree`), exported by git alone (ls-tree and
// cat-file, no tar and no archive, so Windows depends on no tar on PATH), and reports every finding in one run, each with rule,
// file, reason and fix. No network, no cargo, no pnpm, no compile, no broker, no environment, no repository code. A rule that throws
// is a FAIL, never a pass. Nothing is written to the repository.
//
//   eol-policy       G04  a committed blob has CRLF where .gitattributes says eol=lf (every checkout shows it modified;
//                         byte-compared and hashed copies differ on Windows)
//   file-url         G12  the .pathname of a URL built from import.meta.url used as a path: wrong on Windows; use fileURLToPath
//   retired-alias    P11  an invocation of `rightkit`, the retired name of the RustRight client (rustright#91), outside
//                         history; scripts/retire-rightkit-alias.mjs rewrites them (in a RightKit checkout)
//   qa-platform      G13  a Rust `"platform:<x>"` requirement that is not darwin or windows, the words rightkit-qa emits (it skips forever)
//   test-harness     G14  a Cargo.toml [[test]] whose file defines `fn main(` but lacks `harness = false` (libtest ignores it: 0 tests)
//   scenario-skip    G15  a `.scenario(` test that prints SKIP and returns: a skip that reports green
//   release-config   G16  right-release.config.json (or a legacy .mjs): a provider publish-update.mjs refuses, an updater keyFile, an
//                         undefined Cargo profile, JSON right-release cannot load
//   skip-checks      G17  a package.json script that runs right-release with --skip-checks
//   qa-dev-server    G18  a package.json qa* script that runs `tauri dev` (QA would test the dev server, not the app)
//   ffmpeg-release-escape G19 `allow_in_release_builds` or a use of `DevFfmpeg::unverified` (call or path) in non-test Rust
//                         outside crates/rightkit-ffmpeg (an item or file gated on feature "dev-overrides" is excused)
//   switches-inventory G20 a right-release.config.json (or .mjs) directory without a switches.json (or its releaseSwitches.inventory)
//   speech-link      G21  `import Speech` anywhere under swift/RightKitSwift/Sources or Tests (Apple Speech is removed, rightkit#978;
//                         the LinkProbes control fixture is the one sanctioned import)
//   e2e-under-cargo-test G22 `cargo test|run|nextest` (also as `rustright cargo`, or the retired `rightkit`) that runs an end-to-end
//                         crate (rightkit-qa driver modules) in the broker without --no-run: it holds the compile slot for the whole run
//
// Suite trust roots (lane C3; logic in suite-trust-roots.mjs). The licence and updater public keys have one owner,
// contracts/right-suite-api.v1.json (generated into suite-api.generated.mjs):
//   suite-key-literal C3-04  the licence public key pasted as a byte array, 64 hex digits or base64, or the updater key text outside
//                         a tauri.conf.json, in a non-test file
//   suite-updater-key C3-14  a tauri*.conf.json plugins.updater.pubkey that is not a copy of a contract key (missing, a placeholder,
//                         another key, or an unparseable file)
//   engine-record-store C3-05 a call to from_record_store outside crates/rightkit-license: the owner binary hands the engine a
//                         snapshot through rightkit_credentials::handoff instead
// The shell contract (shell-contract.mjs, SHELL_CONTRACT_RULES, appended after e2e-under-cargo-test) reads an app's Tauri config, capabilities,
// Rust and renderer sources:
//   shell-window-hidden      SC-R1  a startup window of any platform config that is not `visible: false`
//   shell-plugin-order       SC-R2  the shell plugin installed from setup, or after a non-QA plugin
//   shell-handler            SC-R3  an invoked rk_shell_* command that no handler registers
//   shell-csp                SC-R4  a CSP without default-src 'self', frame-ancestors 'none', connect-src, or with script unsafe-inline/eval
//   shell-grants             SC-R5  a core:window:allow-* grant that the shell's own window command mirrors
//   qa-window-label          SC-R6  the shell's main_label, the QA Control's window and the created windows disagree
//   devtools-declaration     SC-R7  the five DevTools declaration sites (config, allow-list, Cargo feature, ACL manifest, runtime policy) disagree
//   shell-lifecycle-external SC-R8  lifecycle_only() in an app that also opens an external webview (hardening and navigation policy are off)
//   legal-receipt-native     SC-R9  an Onboarding legal gate whose receipt is not the native service
// Sealed source closures (source-closure-rules.mjs, SOURCE_CLOSURE_RULES, after the shell contract):
//   source-closure           SF-01  a committed JSON record { sourceClosure: { sources: { path: sha256 }, digest } } whose sources are
//                                   missing or changed since sealing, or whose digest does not cover them (normalised digests are
//                                   declared in .rightkit/source-closures.json)
// The ownership class rules (ownership-class-rules.mjs, OWNERSHIP_CLASS_RULES, appended last; code is placed by class, never by
// consumer count):
//   mandatory-foundation     OW-05  an app (root rightkit-mandatory.json, or a release config that ships a desktop app) whose profile
//                                   requires a foundation owner (ownership-mandatory-set.json) it does not use, that selects an unknown
//                                   or unratified profile, that has no declaration, or that excuses a row without a ratified ruling
//   ownership-exceptions     OW-06  a .rightkit/ownership-exceptions.json entry without capability, package, reason, owner, approvedBy,
//                                   date, expires and a RightKit issue, past its expires date (never baselineable), or that no
//                                   ratified ruling (appAmendments.<app>.exceptions) covers
//
// Ratchet: findings that exist today may be listed so they print as KNOWN and do not fail; an unlisted finding fails; a listed
// identity (`${rule}|${file}|${subject}`, no line numbers) that is gone fails as STALE, so the list only shrinks. The baseline is
//   1. --baseline <file>, which must exist; else
//   2. the app's own .rightkit/app-check-baseline.json, read from the checked tree: { schema: 1, known: [{ id, issue, added, sunset }] }.
//      The rule for an entry (APP_BASELINE_RULE; a baseline that breaks it is exit 2, naming the entry): `issue` names the issue that
//      tracks it and is not empty; `added` and `sunset` are real dates, YYYY-MM-DD, UTC; `added` is not later than today; `sunset`
//      is not before `added` and at most 90 days (APP_BASELINE_MAX_DAYS) after it. So an entry silences its finding for at most 90
//      days, the date it counts from cannot be pushed into the future, and renewing one means editing it in a commit. An entry past
//      its sunset date is no longer tolerated and fails as EXPIRED, with its finding; else
//   3. RightKit's central ci/app-check-baseline.json ({ known: { "<repo>": ["<identity>", ...] } }), only when this module is loaded
//      from a RightKit checkout; an installed package has none (its entries keep their own contract: identities only, no dates).
// --print-baseline prints the current identities for a person to paste; the check never writes any baseline.
//
// Exit 0: every rule passed. 1: at least one FAIL, STALE or EXPIRED. 2: the check itself could not run.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { OWNERSHIP_CLASS_RULES } from "./ownership-class-rules.mjs";
import { SOURCE_CLOSURE_RULES } from "./source-closure-rules.mjs";
import { SHELL_CONTRACT_RULES } from "./shell-contract.mjs";
import { engineRecordStore, suiteKeyLiteral, suiteUpdaterKey } from "./suite-trust-roots.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8"); } catch { return null; } };
export const readJson = (root, rel) => { const text = readText(root, rel); try { return text === null ? null : JSON.parse(text); } catch { return null; } };

// git's repository-location variables are dropped from every git call: a hook runs with some of them set, and `--repo` must
// mean the repository it names (the worktree export sets its own GIT_INDEX_FILE).
const GIT_LOCATION = /^GIT_(?:DIR|WORK_TREE|INDEX_FILE|COMMON_DIR|PREFIX|OBJECT_DIRECTORY|ALTERNATE_OBJECT_DIRECTORIES|NAMESPACE)$/i;
const gitEnv = (extra = {}) => ({ ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !GIT_LOCATION.test(key))), ...extra });

function git(args, { cwd, input, env = gitEnv(), encoding = "utf8", maxBuffer = 512 * 1024 * 1024 } = {}) {
  const r = spawnSync("git", args, { cwd, input, env, encoding, maxBuffer });
  if (r.error || r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.error?.message ?? String(r.stderr).trim()}`);
  return r.stdout;
}

/** The git top-level directory of `dir`, or null when it is not inside a work tree. */
export function repoRoot(dir) {
  const r = spawnSync("git", ["rev-parse", "--show-toplevel"], { cwd: dir, env: gitEnv(), encoding: "utf8" });
  return r.error || r.status !== 0 || !String(r.stdout).trim() ? null : path.resolve(String(r.stdout).trim());
}

// ---- rules: each returns [{ problem, fix? }] -----------------------------------------------------------------------------

function eolPolicy({ root, commit, repo = process.cwd() }) {
  // CR-bearing text blobs of the commit, then the commit's own attributes (the exported tree holds its .gitattributes).
  const grep = spawnSync("git", ["grep", "-z", "-l", "-I", "-e", "\r", commit, "--"], { cwd: repo, env: gitEnv(), encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  if (grep.status === 1) return []; // git grep: 1 = no match
  if (grep.error || grep.status !== 0) throw new Error(`git grep failed: ${grep.error?.message ?? grep.stderr.trim()}`);
  const files = grep.stdout.split("\0").filter(Boolean).map((f) => f.slice(commit.length + 1));
  // `-text` (or binary) turns conversion off whatever `eol` says: those bytes are kept as committed (pinned evidence, licence texts).
  const attrs = git(["--work-tree", root, "check-attr", "-z", "--stdin", "text", "eol"], { cwd: repo, input: files.join("\0") }).split("\0");
  const seen = new Map();
  for (let i = 0; i + 2 < attrs.length; i += 3) seen.set(attrs[i], { ...seen.get(attrs[i]), [attrs[i + 1]]: attrs[i + 2] });
  const bad = [...seen].filter(([, a]) => a.eol === "lf" && a.text !== "unset").map(([file]) => file);
  return bad.length ? [{
    problem: `${bad.length} file(s) committed with CRLF although .gitattributes says eol=lf (a fresh checkout shows them modified; hashed and byte-compared copies differ on Windows): ${bad.join(", ")}`,
    fix: `git add --renormalize -- ${bad.join(" ")} && git commit`,
  }] : [];
}

// The RustRight client's subcommands as the retired `rightkit` alias took them (rustright crates/rustright-cli/src/cli.rs).
const ALIAS_SUBCOMMANDS = "cargo|rustc|rustdoc|run|build|owns|status|attach|cancel|list|quiet|queue|prune|deps|profiles|forecast-report|resume-admission|pause-admission|remote|cutover|storage|verify-receipt";
// Each form is [pattern, replacement] or [pattern, replacement, { unlessRetired, lineMust }]: `unlessRetired` spares a line that
// says "retired", where code names the old client on purpose to refuse it (`if (name === "rightkit") refuse("R1-retired-client")`);
// `lineMust` limits a form to lines that also match it (a ternary needs its `?`, so JSON's `"kind": "rightkit"` is data).
// Forms match across lines (a multi-line argv array), so the rule reads whole files, not lines.
const ALIAS_FORMS = [
  // rightkit cargo ..., `rightkit status <id>` (never rightkit-qa, @rightkit/..., .../rightkit, or "rightkit build-control")
  [new RegExp(`(^|[^-./@A-Za-z0-9_$])rightkit(?= (?:${ALIAS_SUBCOMMANDS})(?![-\\w]))`, "gm"), "$1rustright"],
  // ["rightkit", "cargo", ...], its quoting variants, one element per line, and the [command, [args]] pair ["rightkit", ["cargo", ...]]
  [new RegExp(`([\\[(]\\s*)(["'\`])rightkit\\2(?=\\s*,\\s*(?:\\/\\/[^\\n]*\\n\\s*)*(?:\\[\\s*)?["'\`](?:${ALIAS_SUBCOMMANDS})["'\`])`, "g"), "$1$2rustright$2"],
  // the same inside an escaped string (a TOML fixture in a Rust or JS literal): [\"rightkit\", \"cargo\", ...]
  [new RegExp(`([\\[(]\\s*)\\\\(["'])rightkit\\\\\\2(?=\\s*,\\s*\\\\["'](?:${ALIAS_SUBCOMMANDS})\\\\["'])`, "g"), "$1\\$2rustright\\$2"],
  // vec!["rightkit".into(), "cargo".into(), ...] and .to_string() / .to_owned()
  [new RegExp(`([\\[(]\\s*)(["'])rightkit\\2(\\.(?:into|to_string|to_owned)\\(\\))(?=\\s*,\\s*["'](?:${ALIAS_SUBCOMMANDS})["'])`, "g"), "$1$2rustright$2$3"],
  // run("rightkit", ...), spawn("rightkit", ...), execFileSync("rightkit", ...), which("rightkit"), Command::new("rightkit")
  [/\b(run|spawn|spawnSync|execFile|execFileSync|execa|resolveCommand|which|Command::new)\((["'])rightkit\2/g, "$1($2rustright$2"],
  // the Windows shim by name: "rightkit.cmd", `...\agent-bin\rightkit.cmd cargo ...`
  [/(["'])rightkit\.(cmd|exe)\1/g, "$1rustright.$2$1"],
  [new RegExp(`(^|[^-./@A-Za-z0-9_$])rightkit\\.(cmd|exe)(?= (?:${ALIAS_SUBCOMMANDS})(?![-\\w]))`, "gm"), "$1rustright.$2"],
  // isWindows ? "rustright.cmd" : "rightkit" (the POSIX half of the same shim choice)
  [/(["'])rustright\.cmd\1(\s*:\s*)(["'])rightkit\3/g, "$1rustright.cmd$1$2$3rustright$3"],
  // { cmd: "rightkit", args: [...] } in a release config
  [/\bcmd:(\s*)(["'])rightkit\2/g, "cmd:$1$2rustright$2"],
  // a command compared by name: `command === "rightkit"` (a Windows .cmd mapping), `argv0 == "rightkit"`
  [/\b(command|cmd|program|bin|binary|exe|executable|tool|base|first|client|argv0)(\s*(?:===?|!==?)\s*)(["'])rightkit\3/g, "$1$2$3rustright$3", { unlessRetired: true }],
  // the POSIX half of a platform choice: `win32 ? (env.ComSpec ?? "cmd.exe") : "rightkit"`, `isWin ? "x" : "rightkit"`
  [/(\)|["'])(\s*:\s*)(["'])rightkit\3(?=\s*[;,)\n])/g, "$1$2$3rustright$3", { lineMust: /\?/ }],
  // a named command constant: `const CARGO_COMMAND = "rightkit"`, `const BROKER: &str = "rightkit"` (RETIRED_* names are refusals)
  [/\b((?![A-Z0-9_]*RETIRED)[A-Z][A-Z0-9_]*(?:COMMAND|CMD|BIN|BINARY|PROGRAM|EXE|CLI|BROKER|CLIENT))(\s*(?::\s*&(?:'static\s+)?str\s*)?=\s*)(["'])rightkit\3/g, "$1$2$3rustright$3", { unlessRetired: true }],
  // PowerShell and shell lookups and calls: `Get-Command rightkit`, `& rightkit @args`, `which rightkit`, `command -v rightkit`
  [/\b(Get-Command|which|where(?:\.exe)?|command -v)(\s+)rightkit\b(?![-.\w])(?=\s*(?:$|[)|;&>"'`\]]|-ErrorAction))/gm, "$1$2rustright"],
  [/(^|[\s(;{`])&(\s+)rightkit\b(?![-.\w])/gm, "$1&$2rustright"],
  // a per-tool table keyed by the client: `{ rightkit: ["cargo", "--version"] }`
  [/(^|[{,\s])rightkit(\s*:\s*\[)/gm, "$1rustright$2"],
];
/** History and byte-pinned content the alias may stay in: what was run then, or bytes a hash or review binds. */
export const RETIRED_ALIAS_EXEMPT = /(^|\/)CHANGELOG\.md$|^docs\/(audits|plans|archive)\/|(^|\/)(evidence|release-evidence|receipts?|fixtures?|vendor)\/|adr-\d+-evidence\/|\.evidence\/|\d{4}-\d{2}-\d{2}|(^|\/)(handoffs?|handovers?|reports?|results|experiments|reviews|research|mockups|archive|incidents?|spikes)\/|[^/]*evidence[^/]*\/|\.(jsonl|log|patch|diff)$|\.receipt\.json$|(^|\/)SHA256SUMS$|^ci\/consumers\/|^packages\/ax\/plugin\/schemas\/|^scripts\/embed-export\/|msix-repair-guard\.ps1$|\.lock$|^scripts\/(preflight|preflight\.test|retire-rightkit-alias)\.mjs$|^packages\/release\/app-check(\.test)?\.mjs$/;

/** A file generated from other sources (AGENTS.md from docs/agent-rules/): its sources are what is checked and edited. */
export const isGeneratedFile = (text) => /^\s*(?:<!--|\/\/|#)\s*GENERATED FILE\. Do not hand-edit\./.test(String(text).slice(0, 300));

const lineAt = (text, offset) => text.slice(text.lastIndexOf("\n", offset - 1) + 1, (text.indexOf("\n", offset) + 1 || text.length + 1) - 1);

/** `text` with every invocation of the retired alias renamed to `rustright`, how many there were, and the 1-based line of each. */
export function retireAliasText(text) {
  let count = 0;
  let out = text;
  const lines = new Set();
  for (const [pattern, replacement, { unlessRetired = false, lineMust = null } = {}] of ALIAS_FORMS) {
    const hits = [];
    out = out.replace(pattern, (...m) => {
      const offset = m.at(-2);
      const whole = m.at(-1);
      const line = lineAt(whole, offset);
      if ((unlessRetired && /retired/i.test(line)) || (lineMust && !lineMust.test(line))) return m[0];
      count += 1;
      hits.push(offset + m[0].search(/rightkit/));
      return replacement.replace(/\$(\d)/g, (_, i) => m[Number(i)] ?? "");
    });
    // Offsets are positions in the text before this form's replacements; every replacement keeps its line count.
    for (const at of hits) lines.add(text.slice(0, Math.max(0, at)).split("\n").length);
  }
  return { text: out, count, lines: [...lines].sort((a, b) => a - b) };
}

function retiredAlias({ root, files }) {
  const out = [];
  for (const file of files) {
    if (RETIRED_ALIAS_EXEMPT.test(file)) continue;
    let bytes;
    try { bytes = fs.readFileSync(path.join(root, file)); } catch { continue; }
    if (bytes.includes(0) || !bytes.includes("rightkit") || isGeneratedFile(bytes.toString("utf8"))) continue;
    const hits = retireAliasText(bytes.toString("utf8")).lines;
    if (hits.length) out.push({ problem: `${file}:${hits.slice(0, 8).join(",")}${hits.length > 8 ? ",…" : ""} invokes \`rightkit\`, the retired name of the RustRight client`, fix: "use rustright (node <rightkit>/scripts/retire-rightkit-alias.mjs --repo <this repository> rewrites them all)" });
  }
  return out;
}

const FILE_URL_PATHNAME = /new URL\((?:[^()]|\([^()]*\))*import\.meta\.url\s*\)\s*\.pathname/g;
function fileUrls({ root, files }) {
  const out = [];
  for (const file of files) {
    if (!/\.(?:mjs|cjs|js|ts|mts|cts|tsx)$/.test(file) || file.startsWith("vendor/") || file.includes("/node_modules/") || file === "scripts/preflight.mjs") continue;
    const text = readText(root, file);
    if (!text || !text.includes("import.meta.url")) continue;
    for (const m of text.matchAll(FILE_URL_PATHNAME)) {
      const line = text.slice(0, m.index).split("\n").length;
      out.push({ problem: `${file}:${line} turns a file URL into a path with .pathname (C:\\ becomes /C:/ and spaces become %20 on Windows)`, fix: `use fileURLToPath(new URL(..., import.meta.url)) from node:url` });
    }
  }
  return out;
}

// ---- G13-G22: app defects found from files alone ----------------------------------------------------------------------------
// Each finding is { problem, fix, file, subject }; `${rule}|${file}|${subject}` (no line numbers, so an edit above it does not
// change it) is its identity in ci/app-check-baseline.json.
const SKIPPED_TREE = /(^|\/)(node_modules|vendor|target)\//;
const scanned = (file) => !SKIPPED_TREE.test(file);
const lineOf = (text, offset) => text.slice(0, offset).split("\n").length;
/** `text` with every whole-line `//` comment blanked, offsets and line numbers kept (a URL inside a string is untouched). */
const blankComments = (text) => text.replace(/^[ \t]*\/\/.*$/gm, (m) => " ".repeat(m.length));
const rustFiles = (files) => files.filter((f) => f.endsWith(".rs") && scanned(f));
// The release config: the data config (right-release.config.json, M24) or a legacy module (right-release.config.mjs) not yet moved.
const configFiles = (files) => files.filter((f) => /(^|\/)right-release\.config\.(?:mjs|json)$/.test(f) && scanned(f));
const isJsonConfig = (file) => file.endsWith(".json");
/** Every `[key, value, parentKey]` of a parsed JSON value, depth first (the walk the JSON release config rules read). */
function* jsonEntries(node, parentKey = null) {
  if (Array.isArray(node)) { for (const item of node) yield* jsonEntries(item, parentKey); return; }
  if (!node || typeof node !== "object") return;
  for (const [key, value] of Object.entries(node)) { yield [key, value, parentKey]; yield* jsonEntries(value, key); }
}
/** The offset of the first `"key"` in JSON text that is followed by `:` (a key, not a string value), else 0. */
const jsonKeyOffset = (text, key) => Math.max(0, text.search(new RegExp(`"${key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"\\s*:`)));
const dirOf = (file) => path.posix.dirname(file);

// G13: rightkit-qa's util::platform() emits "darwin" or "windows"; a requirement spelled any other way never matches, so the
// test skips on every host and nobody sees it (HeardRight tauri-app-next/qa-e2e/tests/installed_app_ax.rs "platform:macos").
function qaPlatform({ root, files }) {
  const out = [];
  for (const file of rustFiles(files)) {
    const text = readText(root, file);
    if (!text || !text.includes("platform:")) continue;
    // A requirement is an element of a requires list (after `[` or `,`), never a JSON value (`"item": "platform:x"`).
    for (const m of blankComments(text).matchAll(/([\[,]\s*)"platform:([A-Za-z0-9_-]+)"/g)) {
      m.index += m[1].length; m[0] = m[0].slice(m[1].length); m[1] = m[2];
      if (m[1] === "darwin" || m[1] === "windows") continue;
      out.push({ file, subject: `platform:${m[1]}`, problem: `${file}:${lineOf(text, m.index)} requirement ${m[0]} never matches (rightkit-qa platform() is "darwin" or "windows"), so the test skips forever`, fix: `spell it "platform:darwin" or "platform:windows"` });
    }
  }
  return out;
}

/** The [[test]] tables of a Cargo manifest: { name, path, harness, line } (literal values only). */
export function cargoTestTargets(text) {
  const targets = [];
  let current = null;
  String(text ?? "").split("\n").forEach((raw, i) => {
    const line = raw.replace(/\s+#.*$/, "");
    if (/^\s*\[/.test(line)) {
      current = /^\s*\[\[test\]\]\s*$/.test(line) ? { name: undefined, path: undefined, harness: true, line: i + 1 } : null;
      if (current) targets.push(current);
      return;
    }
    if (!current) return;
    const kv = /^\s*([A-Za-z_-]+)\s*=\s*(.*?)\s*$/.exec(line);
    if (!kv) return;
    if (kv[1] === "name" || kv[1] === "path") current[kv[1]] = /^"([^"]*)"$/.exec(kv[2])?.[1];
    else if (kv[1] === "harness") current.harness = kv[2] !== "false";
  });
  return targets;
}

// G14: libtest runs #[test] functions and ignores a `fn main`, so a [[test]] target written as a program but left on the default
// harness passes with 0 tests (CodeRight engine/Cargo.toml installed_matrix_*). An ordinary #[test] file has no top-level main.
const TOP_LEVEL_MAIN = /^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+main\s*\(/m;
function testHarness({ root, files }) {
  const out = [];
  for (const file of files.filter((f) => /(^|\/)Cargo\.toml$/.test(f) && scanned(f))) {
    const text = readText(root, file);
    if (!text || !text.includes("[[test]]")) continue;
    for (const t of cargoTestTargets(text)) {
      if (!t.harness || (!t.name && !t.path)) continue;
      const base = dirOf(file);
      const candidates = t.path ? [path.posix.join(base, t.path)] : [path.posix.join(base, "tests", `${t.name}.rs`), path.posix.join(base, "tests", t.name ?? "", "main.rs")];
      const source = candidates.map((c) => readText(root, c)).find((s) => s !== null);
      if (source === undefined || source === null || !TOP_LEVEL_MAIN.test(blankComments(source))) continue;
      out.push({ file, subject: t.name ?? path.posix.basename(t.path, ".rs"), problem: `${file}:${t.line} [[test]] ${t.name ?? t.path} defines fn main but does not set harness = false: cargo test runs it as a libtest target, ignores main and passes with 0 tests`, fix: "add harness = false to the [[test]] table, or turn main into #[test] functions" });
    }
  }
  return out;
}

// G15: in a `.scenario(` test, `println!("SKIP ...")` then `return;` ends the test green. A scenario that cannot run is a
// failure or an explicit, reviewed ignore (CutRight crates/cutright-ui-e2e/tests/*). The ordinal counts every SKIP print in the
// file, so fixing one does not renumber the others.
function scenarioSkip({ root, files }) {
  const out = [];
  for (const file of rustFiles(files)) {
    const text = readText(root, file);
    if (!text || !text.includes(".scenario(") || !text.includes("SKIP")) continue;
    const lines = text.split("\n");
    let ordinal = 0;
    lines.forEach((line, i) => {
      if (!/\be?println!\s*\(\s*"SKIP/.test(line) || /^\s*\/\//.test(line)) return;
      ordinal += 1;
      if (!lines.slice(i, i + 4).some((l) => /\breturn\s*;/.test(l))) return;
      out.push({ file, subject: `${file}#${ordinal}`, problem: `${file}:${i + 1} prints SKIP and returns inside a scenario test: the skip reports green`, fix: "fail the test (panic or assert) when the scenario cannot run, or mark it #[ignore = \"why\"] so the skip is counted" });
    });
  }
  return out;
}

/** The text of the `{...}` block that opens at `open` (braces in strings are not special-cased), or "" when unbalanced. */
function braceBlock(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === "{") depth += 1;
    else if (text[i] === "}" && --depth === 0) return text.slice(open, i + 1);
  }
  return "";
}

/** Profile names a Cargo manifest defines ([profile.<name>] and [profile.<name>.package...]). */
const cargoProfiles = (text) => new Set([...String(text ?? "").matchAll(/^\s*\[profile\.("[^"]+"|[A-Za-z0-9_-]+)/gm)].map((m) => m[1].replaceAll('"', "")));
const BUILTIN_PROFILES = new Set(["dev", "release", "test", "bench"]);
// The providers packages/release accepts: publish-update.mjs takes the first two for win and mac; the other two are the iOS
// (app-store-connect) and Android (google-play) lanes, whose configs sit in the same file.
const RELEASE_PROVIDERS = new Set(["r2-rightapps", "github-releases", "app-store-connect", "google-play"]);

/** Profiles the nearest workspace Cargo.toml above `dir` defines; with no manifest on the way up to the root, every manifest's. */
function profilesFor(root, files, dir) {
  let first = null;
  for (let d = dir; ; d = dirOf(d)) {
    const text = readText(root, d === "." ? "Cargo.toml" : `${d}/Cargo.toml`);
    if (text !== null) { first ??= text; if (/^\s*\[workspace\]/m.test(text)) return cargoProfiles(text); }
    if (d === ".") break;
  }
  if (first !== null) return cargoProfiles(first);
  return new Set(files.filter((f) => /(^|\/)Cargo\.toml$/.test(f) && scanned(f)).flatMap((f) => [...cargoProfiles(readText(root, f))]));
}

// G16: what a release config says that packages/release refuses later, minutes into a build: a provider publish-update.mjs does not
// know, an updater keyFile (sign-updater.mjs: "--key-file is removed"), a Cargo profile nothing defines (ScrapeRight release-iterate).
function releaseConfig({ root, files }) {
  const out = [];
  for (const file of configFiles(files)) {
    const raw = readText(root, file);
    if (raw === null) continue;
    const text = isJsonConfig(file) ? raw : blankComments(raw);
    const add = (kind, value, offset, why, fix) => out.push({ file, subject: `${kind}:${value}`, problem: `${file}:${lineOf(text, offset)} ${why}`, fix });
    const badProvider = (provider, offset) => add("provider", provider, offset, `distribution provider "${provider}" is refused by packages/release/publish-update.mjs (it takes r2-rightapps or github-releases)`, 'set provider: "r2-rightapps" (private) or "github-releases" (public)');
    const keyFile = (value, offset) => add("keyFile", value.slice(0, 60), offset, "updater keyFile: sign-updater.mjs refuses --key-file; updater keys come from the OS credential provider", "remove keyFile; the updater key is read from the credential store");
    const profiles = [];
    if (isJsonConfig(file)) {
      let config;
      try { config = JSON.parse(raw.replace(/^\uFEFF/, "")); } catch (error) {
        add("json", "invalid", 0, `is not valid JSON (${error.message}): right-release refuses to load it`, "fix the JSON; the release config is strict JSON (no comments or trailing commas)");
        continue;
      }
      for (const [key, value, parent] of jsonEntries(config)) {
        if (key === "provider" && parent === "distribution" && typeof value === "string" && !RELEASE_PROVIDERS.has(value)) badProvider(value, text.indexOf(JSON.stringify(value)));
        if (key === "keyFile" && parent === "updater") keyFile(JSON.stringify(value), jsonKeyOffset(text, "keyFile"));
        if (key === "profile" && (parent === "winPackage" || parent === "macPackage") && typeof value === "string") profiles.push([value, text.indexOf(JSON.stringify(value))]);
      }
    } else {
      for (const m of text.matchAll(/\bprovider\s*:\s*(["'`])([^"'`]+)\1/g)) if (!RELEASE_PROVIDERS.has(m[2])) badProvider(m[2], m.index);
      for (const m of text.matchAll(/\bupdater\s*:\s*\{/g)) {
        const block = braceBlock(text, m.index + m[0].length - 1);
        const key = /\bkeyFile\s*:\s*([^,\n}]+)/.exec(block);
        if (key) keyFile(key[1].trim(), m.index + key.index);
      }
    }
    profiles.push(...[...text.matchAll(/(["'])--profile\1\s*,\s*(["'])([^"']+)\2/g)].map((m) => [m[3], m.index])
      .concat([...text.matchAll(/(["'])--profile=([^"']+)\1/g)].map((m) => [m[2], m.index]))
      .concat([...text.matchAll(/\bPROFILE\s*(?:\?\?|\|\|)\s*(["'])([^"']+)\1/g)].map((m) => [m[2], m.index])));
    if (profiles.length) {
      const defined = profilesFor(root, files, dirOf(file));
      for (const [name, offset] of profiles) {
        if (!BUILTIN_PROFILES.has(name) && !defined.has(name)) add("profile", name, offset, `Cargo profile "${name}" is not dev, release, test or bench and no [profile.${name}] is in the nearest workspace Cargo.toml (cargo refuses it)`, `define [profile.${name}] in the workspace Cargo.toml, or name an existing profile`);
      }
    }
  }
  return out;
}

const packageManifests = (root, files) => files.filter((f) => /(^|\/)package\.json$/.test(f) && scanned(f)).map((file) => ({ file, text: readText(root, file), manifest: readJson(root, file) })).filter((p) => p.manifest?.scripts && typeof p.manifest.scripts === "object");
const scriptLine = (text, name) => lineOf(text, Math.max(0, text.indexOf(JSON.stringify(name))));

// G17: right-release build --skip-checks turns off the gates the release exists to run.
function skipChecks({ root, files }) {
  const out = [];
  for (const { file, text, manifest } of packageManifests(root, files)) {
    for (const [name, command] of Object.entries(manifest.scripts)) {
      if (typeof command === "string" && /\bright-release\b/.test(command) && /(^|\s)--skip-checks(\s|=|$)/.test(command)) out.push({ file, subject: name, problem: `${file}:${scriptLine(text, name)} script "${name}" runs right-release with --skip-checks`, fix: "drop --skip-checks; fix the finding it hides, or record a reviewed exception in the release config" });
    }
  }
  return out;
}

// G18: a debug build without the custom-protocol feature loads the dev server, so `tauri dev` under a qa script tests the dev
// server, not the app that ships. QA runs the built app.
function qaDevServer({ root, files }) {
  const out = [];
  for (const { file, text, manifest } of packageManifests(root, files)) {
    for (const [name, command] of Object.entries(manifest.scripts)) {
      if (/^qa/.test(name) && typeof command === "string" && /\btauri\s+dev\b/.test(command)) out.push({ file, subject: name, problem: `${file}:${scriptLine(text, name)} script "${name}" runs \`tauri dev\`: a debug build without custom-protocol loads the dev server, so QA tests the dev server, not the app`, fix: "run QA against the built app (tauri build --debug with custom-protocol, or the installed app)" });
    }
  }
  return out;
}

// G19: ffmpeg's release gate is the point of crates/rightkit-ffmpeg; allow_in_release_builds and a use of DevFfmpeg::unverified (a call,
// or the path itself, as in .map(DevFfmpeg::unverified)) outside it and its tests open a path that ships a build the gate would have refused. An app pinned to an older rightkit-ffmpeg still
// compiles both, so the names stay refused after the crate stops exporting them. UnverifiedRequest is not named here: it is the
// argument of resolve_ffmpeg_handle(_with), which refuses in release builds, so refusing it would refuse the sanctioned fix.
// DevFfmpeg::try_unverified is the mode-checked constructor and never matches (nor does any other name that merely starts with "unverified").
const DEV_UNVERIFIED = /\bDevFfmpeg\s*::\s*unverified\b/g;
// cfg(feature = "dev-overrides") and cfg(any(test, feature = "dev-overrides")): what they gate is not compiled without the feature.
const DEV_OVERRIDES_PREDICATE = String.raw`(?:feature\s*=\s*"dev-overrides"|any\s*\(\s*(?:test\s*,\s*feature\s*=\s*"dev-overrides"|feature\s*=\s*"dev-overrides"\s*,\s*test)\s*\))`;
const DEV_OVERRIDES_GATE = new RegExp(String.raw`#\s*\[\s*cfg\s*\(\s*${DEV_OVERRIDES_PREDICATE}\s*\)\s*\]`, "g");
// The same gate as an inner attribute that opens the file (only other inner attributes may come before it): the whole file is gated.
const DEV_OVERRIDES_FILE_GATE = new RegExp(String.raw`^(?:\s*#\s*!\s*\[[^\]]*\])*\s*#\s*!\s*\[\s*cfg\s*\(\s*${DEV_OVERRIDES_PREDICATE}\s*\)\s*\]`);
// What starts a gated item or statement (the item rule: ends at its ';' or its closing '}'); anything else is a match arm, a struct field or an expression, which a depth-0 ',' ends too.
const ITEM_START = /^(?:(?:pub|fn|const|static|mod|impl|struct|enum|union|trait|type|use|unsafe|async|extern|let|macro_rules)\b|\{)/;
// A comment or a string or char literal: its text is prose or a message, and its braces are not code.
const NON_CODE = /\/\/[^\n]*|\/\*[\s\S]*?\*\/|b?r(#*)"[\s\S]*?"\1|"(?:\\[\s\S]|[^"\\])*"|'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\n])|[^\\'\n])'/g;
/** Rust `text` with every comment and string or char literal blanked, offsets and line numbers kept. */
const blankNonCode = (text) => text.replace(NON_CODE, (m) => m.replace(/[^\n]/g, " "));

/** [from, to) spans of `text` that a dev-overrides gate covers (`prose` is its blanked copy, same offsets). The gated item is what
 * follows the attribute, stacked attributes included, up to the first `;` outside brackets or the `}` that closes the first `{`:
 * an fn, mod, impl or block, or a `let` or const that ends at its `;`. A gated match arm or struct field (it starts with no item
 * keyword and no `{`) ends at its `,` outside brackets as well, so an arm with no block never excuses the arms after it.
 * Limits: a gate on `mod x;` covers that line only (x.rs is checked on its own, so a call there is still refused); any other
 * predicate (`all(...)`, `not(...)`, another feature) is no gate; an inner `#![cfg(...)]` is not a span (ffmpegReleaseEscape excuses the whole file for it);
 * a `let` whose value goes on after a block (`if a { b } else { c }`) is covered to that block's end; a gated arm or field
 * whose value has a `,` outside brackets (generics such as `f::<A, B>(...)`) is covered to that comma; a tail whose braces
 * never balance is covered to the end of the file. A hit it misses is refused: gate the call itself. */
function devOverrideItems(text, prose) {
  const spans = [];
  for (const gate of text.matchAll(DEV_OVERRIDES_GATE)) {
    if (prose[gate.index] !== "#") continue; // the attribute is itself inside a comment or a string
    let brackets = 0;
    let braces = 0;
    let end = prose.length;
    let first = gate.index + gate[0].length;
    for (let attribute = /^\s*#\s*\[/.exec(prose.slice(first)); attribute; attribute = /^\s*#\s*\[/.exec(prose.slice(first))) {
      let depth = 1;
      first += attribute[0].length;
      for (; first < prose.length && depth > 0; first++) depth += prose[first] === "[" ? 1 : prose[first] === "]" ? -1 : 0;
    }
    const item = ITEM_START.test(prose.slice(first).trimStart());
    for (let i = gate.index + gate[0].length; i < prose.length; i++) {
      const c = prose[i];
      if (c === "(" || c === "[") brackets += 1;
      else if (c === ")" || c === "]") brackets -= 1;
      else if (c === "{") braces += 1;
      else if (c === "}") braces -= 1;
      if (braces < 0 || brackets < 0) { end = i; break; } // the block or call the attribute sits in closes first
      if (!item && c === "," && brackets === 0 && braces === 0) { end = i + 1; break; } // a match arm or field ends at its comma
      if (brackets === 0 && braces === 0 && (c === ";" || c === "}")) { end = i + 1; break; }
    }
    spans.push([gate.index, end]);
  }
  return spans;
}

function ffmpegReleaseEscape({ root, files }) {
  const out = [];
  for (const file of rustFiles(files)) {
    if (/(^|\/)tests\//.test(file) || /(^|\/)(?:tests|[^/]*_tests?)\.rs$/.test(file) || /(^|\/)crates\/rightkit-ffmpeg\//.test(file)) continue;
    const text = readText(root, file);
    if (!text || !/allow_in_release_builds|DevFfmpeg\s*::\s*unverified/.test(text) || /^\s*#!\[cfg\(test\)\]/m.test(text)) continue;
    const testModule = /^#\[cfg\(test\)\]/m.exec(text)?.index ?? Infinity;
    const prose = blankNonCode(text);
    const gated = devOverrideItems(text, prose);
    const allow = [...blankComments(text).matchAll(/allow_in_release_builds/g)].find((m) => m.index < testModule);
    // Only DevFfmpeg::unverified is excused under a dev-overrides gate: allow_in_release_builds is being removed from the crate
    // and from every app, gated or not.
    // A file that opens with the inner attribute #![cfg(feature = "dev-overrides")] is not compiled without the feature at all.
    const fileGated = DEV_OVERRIDES_FILE_GATE.test(text.replace(/\/\/[^\n]*|\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " ")));
    const unverified = fileGated ? undefined : [...prose.matchAll(DEV_UNVERIFIED)].find((m) => m.index < testModule && !gated.some(([from, to]) => m.index >= from && m.index < to));
    const uses = [];
    const fixes = [];
    if (allow) { uses.push(`${file}:${lineOf(text, allow.index)} uses allow_in_release_builds`); fixes.push("remove it from non-test code; only tests and crates/rightkit-ffmpeg itself may name it"); }
    if (unverified) { uses.push(`${file}:${lineOf(text, unverified.index)} uses DevFfmpeg::unverified`); fixes.push('resolve the binary with rightkit_ffmpeg::resolve_ffmpeg_handle, which refuses in release builds; keep DevFfmpeg::unverified in tests or under #[cfg(any(test, feature = "dev-overrides"))] (or #![cfg(...)] at the top of the file)'); }
    if (uses.length) out.push({ file, subject: file, problem: `${uses.join(" and ")} outside test code: a release build could ship what the ffmpeg gate refuses`, fix: fixes.join("; ") });
  }
  return out;
}

// G20: the release-switches scan proves a release binary carries no QA or dev switch, from the app's inventory of them. A
// config without one has nothing to prove against.
function switchesInventory({ root, files }) {
  const out = [];
  for (const file of configFiles(files)) {
    const raw = readText(root, file);
    if (raw === null) continue;
    const text = isJsonConfig(file) ? raw : blankComments(raw);
    const dir = dirOf(file);
    let block;
    let named;
    if (isJsonConfig(file)) {
      let config;
      try { config = JSON.parse(raw.replace(/^\uFEFF/, "")); } catch { config = null; }
      const switches = config?.releaseSwitches;
      if (switches && typeof switches === "object") block = { index: jsonKeyOffset(text, "releaseSwitches") };
      if (typeof switches?.inventory === "string") named = switches.inventory;
    } else {
      block = /\breleaseSwitches\s*:\s*\{/.exec(text);
      named = block ? /\binventory\s*:\s*(["'`])([^"'`]+)\1/.exec(braceBlock(text, block.index + block[0].length - 1))?.[2] : undefined;
    }
    const inventory = path.posix.normalize(path.posix.join(dir, named ?? "switches.json"));
    if (fs.existsSync(path.join(root, inventory))) continue;
    out.push({ file, subject: dir, problem: `${file}:${block ? lineOf(text, block.index) : 1} has no switches inventory (${inventory} does not exist)`, fix: named ? `commit ${inventory} or point releaseSwitches.inventory at the app's inventory` : `add ${path.posix.join(dir, "switches.json")} listing the app's QA and dev switches (see packages/release/fixtures/release-switches/app/switches.json)` });
  }
  return out;
}

// G21: Speech.framework must stay out of RightKitSwift entirely (rightkit#978: Apple Speech is removed; Parakeet and Whisper are the
// only engines). swiftc writes the framework into the object file of every module that imports it, so one `import Speech` in any
// RightKitSwift source or test links it into the apps that take that module (HeardRight's App Store policy). The link probe
// (swift/RightKitSwift/LinkProbes/SpeechNoApple, exempt here: its SpeechControl target is the one sanctioned import) is the proof;
// this is the seconds-fast early signal.
const SPEECH_FREE = /^swift\/RightKitSwift\/(?:Sources|Tests)\/.+\.swift$/;
const IMPORT_SPEECH = /^[ \t]*(?:@\w+(?:\([^)]*\))?[ \t]+)*(?:(?:public|internal|package|private|fileprivate|open)[ \t]+)?import[ \t]+(?:(?:struct|class|enum|protocol|typealias|func|var|let)[ \t]+)?Speech\b/m;
function speechLink({ root, files }) {
  const out = [];
  for (const file of files.filter((f) => SPEECH_FREE.test(f))) {
    const text = readText(root, file);
    if (text === null) continue;
    const hit = IMPORT_SPEECH.exec(blankComments(text));
    if (hit) out.push({ file, subject: file, problem: `${file}:${lineOf(text, hit.index)} imports Speech: RightKitSwift must not link Speech.framework (Apple Speech was removed, rightkit#978)`, fix: "delete the import; Parakeet and Whisper are the only speech engines" });
  }
  return out;
}

// G22: the broker compiles; it does not run end-to-end tests. `cargo test`, `cargo run` and `cargo nextest` (plain, through the
// RustRight client as `rustright cargo`, or as `rightkit` while the retired alias lasts) run the test binary or the program inside the
// broker job, which holds the compile slot for the whole run (Pulse, 2026-10-23: 55 minutes with 15 builds queued) and has no GUI
// session or unscrubbed environment a real-app test needs. The fix is `--no-run` to compile and `rightkit-qa run-tests` to run
// outside the broker (crates/rightkit-qa/docs/run-tests.md). Found from files alone, in three steps:
//   1. the end-to-end crates: a Cargo package that depends on rightkit-qa not optionally (the optional dependency is the in-app control
//      server behind a qa-native / dev-control feature) and has a .rs file, outside examples/, that names a driver module of it;
//   2. the commands: `cargo test|run|nextest` in a package.json script, a shell, PowerShell, batch, Node or Python script, a Makefile
//      or justfile recipe, a GitHub workflow `run:` step, a git hook, or a code span of a runbook document;
//   3. the selection: a command that is not `--no-run` or a rightkit-qa run-tests / journeys run, and names an end-to-end crate by
//      -p / --package, --manifest-path, --workspace / --all (a member of the workspace in the working directory), --test or --bin.
// Known limits, deliberate for an offline check: no cargo binary (a path dependency that cargo adds to a workspace implicitly is not
// a member here), a command assembled from variables is matched on its literal text only, a package.json script that calls another
// script is judged where the cargo text is, a cargo alias (`cargo t`) is not matched, and a bare `cargo test` selects nothing.
const E2E_DRIVER_MODULES = new Set(["harness", "control", "suite", "scenario", "engine", "in_app", "interactive", "page", "browser", "android"]);
const E2E_GUARD_TARGET = "qa_surface_guard"; // the one test target of an end-to-end crate that only runs the static guard
const E2E_NAME_ONLY = /-e2e$|^qa-/; // a name is not evidence: crates that match only this are listed for a reviewer by e2eCrates()

const unquote = (text) => String(text).replace(/^(["'])(.*)\1$/, "$2");
const escapeRegExp = (text) => String(text).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
/** A dotted TOML key split at the dots outside quotes: `target.'cfg(unix)'.dependencies` is three parts. */
const tomlPath = (text) => [...String(text).matchAll(/"([^"]*)"|'([^']*)'|([^\s.'"]+)/g)].map((m) => m[1] ?? m[2] ?? m[3]);
const tomlStrings = (text) => [...String(text).matchAll(/"([^"]*)"|'([^']*)'/g)].map((m) => m[1] ?? m[2]);
const uncomment = (line) => line.replace(/^\s*#.*$/, "").replace(/\s+#.*$/, "");

/**
 * What the end-to-end check reads from a Cargo manifest, literal values only: the package name, whether it holds a [workspace] and its
 * members and exclude, the dependencies of [dependencies], [dev-dependencies] and their [target.*] tables (inline, dotted-key and
 * `[dependencies.name]` forms, each with optional, package and workspace), [workspace.dependencies] as key -> package, and the
 * [[test]] and [[bin]] tables. Build dependencies are not dependencies of the tests.
 */
function parseCargoManifest(text) {
  const manifest = { name: null, workspace: false, members: [], exclude: [], dependencies: [], workspaceDependencies: new Map(), tests: [], bins: [] };
  const entries = new Map();
  const dependency = (table, key) => {
    const id = `${table}\0${key}`;
    if (!entries.has(id)) { const entry = { key, optional: false, package: undefined, workspace: false }; entries.set(id, entry); manifest.dependencies.push(entry); }
    return entries.get(id);
  };
  const setAttribute = (entry, attribute, value) => {
    if (attribute === "optional") entry.optional = entry.optional || value === "true";
    else if (attribute === "package") entry.package = tomlStrings(value)[0] ?? entry.package;
    else if (attribute === "workspace") entry.workspace = entry.workspace || value === "true";
  };
  const setInline = (entry, value) => {
    if (/\boptional\s*=\s*true\b/.test(value)) entry.optional = true;
    const renamed = /\bpackage\s*=\s*(?:"([^"]+)"|'([^']+)')/.exec(value);
    if (renamed) entry.package = renamed[1] ?? renamed[2];
    if (/\bworkspace\s*=\s*true\b/.test(value)) entry.workspace = true;
  };
  const lines = String(text ?? "").split("\n");
  let section = { kind: "other" };
  for (let i = 0; i < lines.length; i++) {
    const line = uncomment(lines[i]);
    const header = /^\s*(\[\[?)\s*(.+?)\s*\]\]?\s*$/.exec(line);
    if (header) {
      const parts = tomlPath(header[2]);
      if (header[1] === "[[") {
        const item = parts[0] === "test" || parts[0] === "bin" ? { name: undefined, path: undefined } : null;
        if (item) manifest[parts[0] === "test" ? "tests" : "bins"].push(item);
        section = item ? { kind: "target", item } : { kind: "other" };
        continue;
      }
      const at = parts[0] === "dependencies" || parts[0] === "dev-dependencies" ? 0 : parts[0] === "target" && (parts[2] === "dependencies" || parts[2] === "dev-dependencies") ? 2 : -1;
      if (parts[0] === "package" && parts.length === 1) section = { kind: "package" };
      else if (parts[0] === "workspace") {
        manifest.workspace = true;
        section = parts.length === 1 ? { kind: "workspace" } : parts[1] === "dependencies" ? (parts.length === 2 ? { kind: "workspace-dependencies" } : { kind: "workspace-dependency", key: parts[2] }) : { kind: "other" };
      } else if (at >= 0) section = parts.length === at + 1 ? { kind: "dependencies", table: parts.join(".") } : { kind: "dependency", table: parts.slice(0, at + 1).join("."), key: parts[at + 1] };
      else section = { kind: "other" };
      continue;
    }
    const kv = /^\s*("[^"]+"|'[^']+'|[A-Za-z0-9_-]+)(?:\s*\.\s*("[^"]+"|'[^']+'|[A-Za-z0-9_-]+))?\s*=\s*(.*?)\s*$/.exec(line);
    if (!kv) continue;
    const key = unquote(kv[1]);
    const attribute = kv[2] === undefined ? undefined : unquote(kv[2]);
    const value = kv[3];
    switch (section.kind) {
      case "package":
        if (key === "name" && attribute === undefined) manifest.name = tomlStrings(value)[0] ?? null;
        break;
      case "workspace":
        if ((key === "members" || key === "exclude") && attribute === undefined) {
          let list = value;
          if (list.startsWith("[")) while (!list.includes("]") && i + 1 < lines.length) list += ` ${uncomment(lines[++i])}`;
          manifest[key].push(...tomlStrings(list));
        }
        break;
      case "target":
        if ((key === "name" || key === "path") && attribute === undefined) section.item[key] = tomlStrings(value)[0];
        break;
      case "dependencies":
        if (attribute === undefined) setInline(dependency(section.table, key), value); else setAttribute(dependency(section.table, key), attribute, value);
        break;
      case "dependency":
        if (attribute === undefined) setAttribute(dependency(section.table, section.key), key, value);
        break;
      case "workspace-dependencies":
        if (attribute === undefined) manifest.workspaceDependencies.set(key, /\bpackage\s*=\s*(?:"([^"]+)"|'([^']+)')/.exec(value)?.slice(1).find(Boolean) ?? key);
        else if (attribute === "package") manifest.workspaceDependencies.set(key, tomlStrings(value)[0] ?? key);
        break;
      case "workspace-dependency":
        if (key === "package" && attribute === undefined) manifest.workspaceDependencies.set(section.key, tomlStrings(value)[0] ?? section.key);
        else if (!manifest.workspaceDependencies.has(section.key)) manifest.workspaceDependencies.set(section.key, section.key);
        break;
      default:
    }
  }
  return manifest;
}

/** A `[workspace] members` pattern (`crates/*`, `apps/**`, `./x`) as a regular expression over a posix path; a pattern that is no
 * valid glob matches nothing (the rule must not throw on a manifest cargo would refuse). */
function memberPattern(glob) {
  const source = String(glob).replace(/^\.\//, "").replace(/\/+$/, "").split("**").map((part) => part.replace(/[.+^$(){}|\\]/g, "\\$&").replace(/\*/g, "[^/]*").replace(/\?/g, "[^/]")).join(".*");
  try { return new RegExp(`^${source}$`); } catch { return /$^/; }
}

/** True when the Rust source names a driver module of rightkit-qa through any of `names` (its crate identifier(s) in this package):
 * `rightkit_qa::harness`, or a `use rightkit_qa::{...}` list with a driver module among its top-level items. Comments do not count. */
function namesDriverModule(source, names) {
  const text = source.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " ")).replace(/\/\/[^\n]*/g, "");
  for (const name of names) {
    for (const m of text.matchAll(new RegExp(`(?<!\\w)${escapeRegExp(name)}\\s*::\\s*(\\{|[A-Za-z_]\\w*)`, "g"))) {
      if (m[1] !== "{") { if (E2E_DRIVER_MODULES.has(m[1])) return true; continue; }
      const list = braceBlock(text, m.index + m[0].length - 1).slice(1, -1);
      let depth = 0, item = "";
      const items = [];
      for (const ch of list) {
        if (ch === "{") depth += 1; else if (ch === "}") depth -= 1;
        if (ch === "," && depth === 0) { items.push(item); item = ""; } else item += ch;
      }
      items.push(item);
      if (items.some((it) => E2E_DRIVER_MODULES.has(/^\s*([A-Za-z_]\w*)/.exec(it)?.[1]))) return true;
    }
  }
  return false;
}

/**
 * The Cargo layout of a tree and the end-to-end crates in it. `crates` are the end-to-end ones as { name, dir, tests, bins } (a
 * package that depends on rightkit-qa not optionally and has a non-example .rs file naming one of its driver modules); `nameOnly`
 * lists packages named like one (`*-e2e`, `qa-*`) that are not, for a reviewer: a name is not evidence.
 */
export function e2eCrates(root, files) {
  const manifests = new Map();
  for (const file of files.filter((f) => /(^|\/)Cargo\.toml$/.test(f) && scanned(f))) {
    const text = readText(root, file);
    if (text !== null) manifests.set(dirOf(file), parseCargoManifest(text));
  }
  /** The package directory a file belongs to: the nearest directory above it with a Cargo.toml, when that holds a [package]. */
  const ownerOf = (file) => {
    for (let dir = dirOf(file); ; dir = dirOf(dir)) {
      const manifest = manifests.get(dir);
      if (manifest) return manifest.name === null ? null : dir;
      if (dir === ".") return null;
    }
  };
  const packageOf = (dep, dir) => {
    if (dep.package) return dep.package;
    if (dep.workspace) {
      for (let d = dir; ; d = dirOf(d)) {
        const inherited = manifests.get(d)?.workspaceDependencies.get(dep.key);
        if (inherited) return inherited;
        if (d === ".") break;
      }
    }
    return dep.key;
  };
  const identifiers = new Map(); // package dir -> how Rust code names rightkit-qa there (the key, with - as _)
  for (const [dir, manifest] of manifests) {
    if (manifest.name === null) continue;
    const names = manifest.dependencies.filter((dep) => !dep.optional && packageOf(dep, dir) === "rightkit-qa").map((dep) => dep.key.replaceAll("-", "_"));
    if (names.length) identifiers.set(dir, [...new Set(names)]);
  }
  const driving = new Set();
  if (identifiers.size) {
    for (const file of rustFiles(files)) {
      const dir = ownerOf(file);
      if (dir === null || !identifiers.has(dir) || driving.has(dir) || (dir === "." ? file : file.slice(dir.length + 1)).startsWith("examples/")) continue;
      const text = readText(root, file);
      if (text !== null && identifiers.get(dir).some((name) => text.includes(name)) && namesDriverModule(text, identifiers.get(dir))) driving.add(dir);
    }
  }
  const crates = [...driving].sort().map((dir) => {
    const manifest = manifests.get(dir);
    const within = (file) => (dir === "." ? file : file.startsWith(`${dir}/`) ? file.slice(dir.length + 1) : null);
    const tests = new Set(manifest.tests.map((t) => t.name).filter(Boolean));
    const bins = new Set(manifest.bins.map((b) => b.name).filter(Boolean));
    for (const file of files) {
      const rel = within(file);
      if (rel === null || !scanned(file) || ownerOf(file) !== dir) continue;
      const test = /^tests\/([^/]+)(?:\.rs|\/main\.rs)$/.exec(rel);
      if (test) tests.add(test[1]);
      const bin = /^src\/bin\/([^/]+)(?:\.rs|\/main\.rs)$/.exec(rel);
      if (bin) bins.add(bin[1]);
      if (rel === "src/main.rs") bins.add(manifest.name);
    }
    return { name: manifest.name, dir, tests, bins };
  });
  const nameOnly = [...manifests].filter(([dir, m]) => m.name !== null && E2E_NAME_ONLY.test(m.name) && !driving.has(dir)).map(([, m]) => m.name).sort();
  /** The end-to-end crates that are members of the workspace rooted at `wsDir`: its root package, and every crate that matches a
   * `members` glob, is not under `exclude` and is not a workspace root itself (a nested [workspace] makes a crate its own root). */
  const membersOf = (wsDir) => {
    const ws = manifests.get(wsDir);
    if (!ws?.workspace) return [];
    const included = ws.members.map(memberPattern);
    const excluded = ws.exclude.map((x) => String(x).replace(/^\.\//, "").replace(/\/+$/, ""));
    return crates.filter((c) => {
      if (c.dir === wsDir) return true;
      const rel = wsDir === "." ? c.dir : c.dir.startsWith(`${wsDir}/`) ? c.dir.slice(wsDir.length + 1) : null;
      return rel !== null && !manifests.get(c.dir).workspace && included.some((re) => re.test(rel)) && !excluded.some((x) => rel === x || rel.startsWith(`${x}/`));
    });
  };
  return { crates, nameOnly, manifests, membersOf };
}

const CARGO_COMMAND = /(?<![\w-])((?:(?:rustright|rightkit)\s+)?cargo)\s+(?:\+\S+\s+)?(test|run|nextest)\b([^&;|`\n]*)/g;
const CD_COMMAND = /(?<![\w./-])(?:cd|Set-Location)\s+(?:-(?:Path|LiteralPath)\s+)?("[^"]*"|'[^']*'|[^\s;&|)]+)/gi;
const COMMENT_LINE = /^\s*(?:#|\/\/|\/?\*|;|::|REM\b)/i;
const QA_RUNNER = /\brightkit-qa(?:\.exe)?\s+(?:run-tests|journeys\s+run)\b/;
const NEXTEST_NO_TESTS = new Set("list archive show-config self help".split(" ")); // nextest subcommands that run no test
const HISTORY_DOC = /^docs\/(?:archive|plans|provenance|handoff[^/]*)\/|(?:^|\/)(?:tasks|evidence)\/|(?:^|\/)(?:changelog|pending|status|state)\.md$/i;
const HOOK_FILE = /(?:^|\/)(?:\.githooks|\.husky|githooks)\/(?!_\/)[^/]+/;
const WORKFLOW_FILE = /^\.github\/(?:workflows\/[^/]+|actions\/.+\/action)\.ya?ml$/;
const BUILD_FILE = /(?:^|\/)(?:Makefile|makefile|GNUmakefile|[Jj]ustfile|\.justfile)$|\.mk$/;
const SCRIPT_FILE = /\.(?:sh|bash|zsh|ps1|psm1|bat|cmd|mjs|cjs|js|py)$/i;

/** A script's `cd` / `Set-Location` target applied to `cwd` (a posix path from the repository root). A target that is not a plain
 * relative path (a variable, a substitution, an absolute path, one that leaves the repository) leaves the directory as it was: the
 * `cd "$(dirname "$0")/.."` of a script under scripts/ is the repository root, where a script starts. */
function changeDirectory(cwd, target) {
  const t = unquote(target);
  if (!t || /[$`(%~]/.test(t) || /^([\\/]|[A-Za-z]:)/.test(t) || t === "-") return cwd;
  const next = path.posix.normalize(path.posix.join(cwd, t.replaceAll("\\", "/")));
  return next === ".." || next.startsWith("../") ? cwd : next;
}

/** The logical lines of physical `lines` ({ no, text }): comment lines dropped, and a line that ends with `continuation` joined to the
 * next, numbered by the line the command starts on. */
function logicalLines(lines, continuation) {
  const out = [];
  let pending = null;
  for (const line of lines) {
    if (!pending && COMMENT_LINE.test(line.text)) continue;
    const text = line.text.replace(/\s+$/, "");
    const joined = pending ? `${pending.text} ${text.trim()}` : text;
    const no = pending?.no ?? line.no;
    if (continuation && text.endsWith(continuation)) { pending = { no, text: joined.slice(0, -continuation.length) }; continue; }
    pending = null;
    out.push({ no, text: joined });
  }
  if (pending) out.push(pending);
  return out;
}

/** The `run:` steps of a GitHub workflow or composite action: { lines, cwd } where cwd is the step's working-directory (the
 * repository root when it has none, or one that is an expression). */
function workflowSteps(text) {
  const lines = text.split("\n");
  const indent = (l) => l.length - l.trimStart().length;
  const steps = [];
  lines.forEach((raw, i) => {
    const m = /^(\s*)(-\s+)?run:\s*(.*?)\s*$/.exec(raw);
    if (!m) return;
    const keyColumn = m[1].length + (m[2]?.length ?? 0);
    const value = m[3];
    let block;
    if (/^[|>][+-]?\d*(?:\s+#.*)?$/.test(value)) {
      block = [];
      for (let j = i + 1; j < lines.length; j++) {
        if (lines[j].trim() === "") continue;
        if (indent(lines[j]) <= keyColumn) break;
        block.push({ no: j + 1, text: lines[j].trim() });
      }
      // A folded scalar (`>`) is one line to the shell.
      if (value.startsWith(">") && block.length) block = [{ no: block[0].no, text: block.map((l) => l.text).join(" ") }];
    } else {
      const plain = /^(["'])(.*)\1$/.test(value) ? unquote(value) : value.replace(/\s+#.*$/, "");
      block = plain ? [{ no: i + 1, text: plain }] : [];
    }
    // The step's own working-directory sits beside `run:` (same column), on either side of it.
    const sibling = (j) => { const k = /^(\s*)(-\s+)?working-directory:\s*(.*?)\s*$/.exec(lines[j]); return k && k[1].length + (k[2]?.length ?? 0) === keyColumn ? unquote(k[3].replace(/\s+#.*$/, "")) : null; };
    let directory = null;
    for (let j = i - 1; j >= 0 && directory === null; j--) {
      if (lines[j].trim() === "") continue;
      directory = sibling(j);
      if (indent(lines[j]) < keyColumn) break;
    }
    for (let j = i + 1; j < lines.length && directory === null; j++) {
      if (lines[j].trim() === "") continue;
      if (indent(lines[j]) < keyColumn) break;
      directory = sibling(j);
    }
    steps.push({ lines: block, cwd: directory === null ? "." : changeDirectory(".", directory) });
  });
  return steps;
}

/** The command-bearing units of a runbook document: each fenced code block (its lines) and each inline code span (one line). Prose
 * outside code is never a command. */
function markdownUnits(text) {
  const units = [];
  let fence = null;
  text.split("\n").forEach((raw, i) => {
    if (fence) {
      const close = /^\s*(`{3,}|~{3,})\s*$/.exec(raw);
      if (close && close[1][0] === fence.char && close[1].length >= fence.length) { units.push(fence.lines); fence = null; } else fence.lines.push({ no: i + 1, text: raw });
      return;
    }
    const open = /^\s*(`{3,}|~{3,})/.exec(raw);
    if (open) { fence = { char: open[1][0], length: open[1].length, lines: [] }; return; }
    for (const span of raw.matchAll(/(`+)(.+?)\1(?!`)/g)) units.push([{ no: i + 1, text: span[2].trim() }]);
  });
  if (fence) units.push(fence.lines);
  return units;
}

// G22: see the block comment above.
function e2eUnderCargoTest({ root, files }) {
  const layout = e2eCrates(root, files);
  if (!layout.crates.length) return [];
  const out = [];
  const seen = new Set();
  const flagValues = (tokens, ...names) => tokens.flatMap((t, i) => names.flatMap((name) => (t === name ? (i + 1 < tokens.length ? [tokens[i + 1]] : []) : name.startsWith("--") && t.startsWith(`${name}=`) ? [t.slice(name.length + 1)] : [])));
  /** The directory of the manifest a --manifest-path names (relative to the cwd, then to the repository root), when it holds one. */
  const manifestDirectory = (value, cwd) => {
    const base = value.replaceAll("\\", "/").replace(/(^|\/)Cargo\.toml$/, "") || ".";
    if (path.posix.isAbsolute(base) || /^[A-Za-z]:/.test(base)) return null;
    for (const from of [cwd, "."]) {
      const dir = path.posix.normalize(path.posix.join(from, base));
      if (dir !== ".." && !dir.startsWith("../") && layout.manifests.has(dir)) return dir;
    }
    return null;
  };
  /** The end-to-end crates a cargo command selects, each with the flag that did it: [{ crate, selector }], first rule first. */
  const selected = (tokens, isRun, cwd) => {
    const picks = new Map();
    const pick = (crate, selector) => { if (!picks.has(crate)) picks.set(crate, selector); };
    const packages = flagValues(tokens, "--package", "-p");
    const manifestPaths = flagValues(tokens, "--manifest-path");
    const tests = flagValues(tokens, "--test");
    const wholeWorkspace = tokens.includes("--workspace") || tokens.includes("--all");
    const excluded = new Set(flagValues(tokens, "--exclude"));
    const guardOnly = tests.length > 0 && tests.every((t) => t === E2E_GUARD_TARGET); // `--test qa_surface_guard` runs the static guard only
    const members = (dir) => layout.membersOf(dir).filter((c) => !excluded.has(c.name));
    if (!guardOnly) {
      for (const p of packages) for (const c of layout.crates.filter((crate) => crate.name === p)) pick(c, `-p ${p}`);
      for (const mp of manifestPaths) {
        const dir = manifestDirectory(mp, cwd);
        if (dir === null) continue;
        for (const c of layout.crates.filter((crate) => crate.dir === dir)) pick(c, `--manifest-path ${mp}`);
        if (wholeWorkspace) for (const c of members(dir)) pick(c, `--manifest-path ${mp} ${tokens.includes("--workspace") ? "--workspace" : "--all"}`);
      }
      if (wholeWorkspace && !manifestPaths.length) for (const c of members(cwd)) pick(c, tokens.includes("--workspace") ? "--workspace" : "--all");
    }
    if (!packages.length && !manifestPaths.length) {
      for (const t of tests) {
        const owners = t === E2E_GUARD_TARGET ? [] : layout.crates.filter((c) => c.tests.has(t));
        if (owners.length === 1) pick(owners[0], `--test ${t}`);
      }
    }
    if (isRun) for (const b of flagValues(tokens, "--bin")) for (const c of layout.crates.filter((crate) => crate.bins.has(b))) pick(c, `--bin ${b}`);
    return [...picks].map(([crate, selector]) => ({ crate, selector }));
  };
  /** Scan logical lines ({ no, text }) that run in order from `cwd`; `what` names the place and `subject` overrides the crate name in the identity. */
  const scan = (file, lines, cwd, what, subject) => {
    for (const { no, text } of lines) {
      if (!/cargo|\bcd\b|Set-Location/i.test(text)) continue; // a line with neither a command nor a directory change cannot matter
      const events = [...text.matchAll(CD_COMMAND)].map((m) => ({ at: m.index, cd: m[1] })).concat([...text.matchAll(CARGO_COMMAND)].map((m) => ({ at: m.index, m })));
      for (const event of events.sort((a, b) => a.at - b.at)) {
        if (event.cd !== undefined) { cwd = changeDirectory(cwd, event.cd); continue; }
        const [, client, verb] = event.m;
        // A command inside a string literal ends at the quote that closes it: `run("rustright cargo test -p x")`.
        let rest = event.m[3];
        for (const quote of ['"', "'"]) {
          if ((rest.split(quote).length - 1) % 2 === 1 && (text.slice(0, event.at).split(quote).length - 1) % 2 === 1) rest = rest.slice(0, rest.indexOf(quote));
        }
        const tokens = (rest.match(/(?:[^\s"']+|"[^"]*"|'[^']*')+/g) ?? []).map((t) => t.replace(/["']/g, "").replace(/\)+$/, ""));
        const ownArguments = tokens.indexOf("--") < 0 ? tokens : tokens.slice(0, tokens.indexOf("--"));
        const segment = text.slice(Math.max(text.lastIndexOf("&", event.at), text.lastIndexOf(";", event.at), text.lastIndexOf("|", event.at), text.lastIndexOf("`", event.at)) + 1, event.at + event.m[0].length);
        if (ownArguments.includes("--no-run") || QA_RUNNER.test(segment)) continue;
        if (ownArguments.some((t) => t === "--help" || t === "-h" || t === "--version" || t === "-V")) continue;
        if (verb === "nextest" && NEXTEST_NO_TESTS.has(tokens[0])) continue;
        for (const { crate, selector } of selected(ownArguments, verb === "run", cwd)) {
          const key = `${file}\0${no}\0${crate.name}`;
          if (seen.has(key)) continue;
          seen.add(key);
          const via = `${client.replace(/\s+/g, " ")} ${verb} ${selector}`;
          out.push({ file, subject: subject ?? crate.name, problem: `${file}:${no} ${what} runs the end-to-end crate ${crate.name} under cargo test (${via}): the test binary runs in the broker's context and holds its compile slot for the whole run`, fix: `compile it with --no-run and run it outside the broker: rightkit-qa run-tests -p ${crate.name} [--test <target>] -- <libtest args>` });
        }
      }
    }
  };
  for (const { file, text, manifest } of packageManifests(root, files)) {
    for (const [name, command] of Object.entries(manifest.scripts)) {
      if (typeof command === "string") scan(file, [{ no: scriptLine(text, name), text: command }], dirOf(file), `script "${name}"`, name);
    }
  }
  for (const file of files.filter(scanned)) {
    const base = path.posix.basename(file);
    const runbook = (file === "README.md" || file === "AGENTS.md" || file === "CLAUDE.md" || /^docs\/.+\.md$/.test(file)) && !HISTORY_DOC.test(file) && !/\d{4}-\d{2}-\d{2}/.test(base);
    const workflow = WORKFLOW_FILE.test(file);
    const hook = !workflow && HOOK_FILE.test(file);
    const recipe = !workflow && !hook && BUILD_FILE.test(file);
    const script = !workflow && !hook && !recipe && SCRIPT_FILE.test(file);
    if (!(runbook || workflow || hook || recipe || script) || base === "package.json") continue;
    const content = readText(root, file);
    if (content === null || !/cargo/.test(content)) continue;
    const continuation = /\.(?:ps1|psm1)$/i.test(file) ? "`" : /\.(?:bat|cmd)$/i.test(file) ? "^" : "\\";
    const physical = content.split("\n").map((line, i) => ({ no: i + 1, text: line }));
    if (workflow) for (const step of workflowSteps(content)) scan(file, logicalLines(step.lines, continuation), step.cwd, "workflow step");
    else if (runbook) for (const unit of markdownUnits(content)) scan(file, logicalLines(unit, continuation), ".", "runbook command");
    else scan(file, logicalLines(physical, continuation), ".", hook ? "git hook" : recipe ? "recipe" : "command");
  }
  return out;
}

export const RULES = Object.freeze([
  { id: "eol-policy", gap: "G04", run: eolPolicy },
  { id: "file-url", gap: "G12", run: fileUrls },
  { id: "retired-alias", gap: "P11", run: retiredAlias },
  { id: "qa-platform", gap: "G13", run: qaPlatform },
  { id: "test-harness", gap: "G14", run: testHarness },
  { id: "scenario-skip", gap: "G15", run: scenarioSkip },
  { id: "release-config", gap: "G16", run: releaseConfig },
  { id: "skip-checks", gap: "G17", run: skipChecks },
  { id: "qa-dev-server", gap: "G18", run: qaDevServer },
  { id: "ffmpeg-release-escape", gap: "G19", run: ffmpegReleaseEscape },
  { id: "switches-inventory", gap: "G20", run: switchesInventory },
  { id: "speech-link", gap: "G21", run: speechLink },
  { id: "e2e-under-cargo-test", gap: "G22", run: e2eUnderCargoTest },
  { id: "suite-key-literal", gap: "C3-04", run: ({ root, files }) => suiteKeyLiteral(root, files) },
  { id: "suite-updater-key", gap: "C3-14", run: ({ root, files }) => suiteUpdaterKey(root, files) },
  { id: "engine-record-store", gap: "C3-05", run: ({ root, files }) => engineRecordStore(root, files) },
  ...SHELL_CONTRACT_RULES,
  ...SOURCE_CLOSURE_RULES,
  ...OWNERSHIP_CLASS_RULES,
]);

// ---- export: the committed files, by git alone ----------------------------------------------------------------------------------

/** The path a tree entry is written to. A tree is untrusted bytes: nothing may land outside `dest` or inside a `.git`. */
export function exportTarget(dest, rel) {
  const parts = rel.split("/");
  const unsafe = !rel || rel.startsWith("/") || rel.includes("\0") || parts.some((s) => s === "" || s === "." || s === ".." || s.toLowerCase() === ".git")
    || (process.platform === "win32" && /[\\:]/.test(rel));
  if (unsafe) throw new Error(`refusing to export the unsafe path ${JSON.stringify(rel)}`);
  return path.join(dest, ...parts);
}

const BATCH_BYTES = 64 * 1024 * 1024;

/**
 * Write every file of `commit` (a commit or tree id) under `dest`: `git ls-tree -r -z -l` names them, `git cat-file --batch`
 * returns the bytes (in batches of at most `batchBytes`, 64 MiB), node fs writes them. The bytes are the committed blobs: no smudge filter, no
 * eol conversion and no export-ignore, so every host sees the same tree. A symlink becomes a file holding its target (as
 * core.symlinks=false does) and a submodule an empty directory. Returns the repository paths as `ls-tree` lists them.
 */
export function exportCommit(repo, commit, dest, { batchBytes = BATCH_BYTES } = {}) {
  const entries = git(["ls-tree", "-r", "-z", "-l", commit], { cwd: repo }).split("\0").filter(Boolean).map((record) => {
    const tab = record.indexOf("\t");
    const m = /^(\d+) (\w+) ([0-9a-f]{40,64}) +(\d+|-)$/.exec(record.slice(0, Math.max(0, tab)));
    if (!m) throw new Error(`unexpected git ls-tree output: ${JSON.stringify(record.slice(0, 120))}`);
    return { mode: m[1], type: m[2], oid: m[3], size: m[4] === "-" ? 0 : Number(m[4]), path: record.slice(tab + 1) };
  });
  fs.mkdirSync(dest, { recursive: true });
  const blobs = new Map();
  for (const entry of entries) {
    const target = exportTarget(dest, entry.path);
    if (entry.type === "commit") { fs.mkdirSync(target, { recursive: true }); continue; } // a submodule: git archive makes the directory too
    if (entry.type !== "blob") throw new Error(`unexpected ${entry.type} ${JSON.stringify(entry.path)} in the tree`);
    if (!blobs.has(entry.oid)) blobs.set(entry.oid, { size: entry.size, targets: [] });
    blobs.get(entry.oid).targets.push({ target, mode: entry.mode === "100755" && process.platform !== "win32" ? 0o755 : 0o644 });
  }
  const oids = [...blobs.keys()];
  for (let from = 0; from < oids.length;) {
    let to = from, bytes = 0;
    while (to < oids.length && (to === from || bytes + blobs.get(oids[to]).size <= batchBytes)) bytes += blobs.get(oids[to++]).size;
    const batch = oids.slice(from, to);
    from = to;
    const out = git(["cat-file", "--batch"], { cwd: repo, input: Buffer.from(`${batch.join("\n")}\n`), encoding: "buffer", maxBuffer: bytes + batch.length * 128 + 1024 * 1024 });
    let at = 0;
    for (const oid of batch) {
      const eol = out.indexOf(10, at);
      const header = eol < 0 ? "" : out.toString("latin1", at, eol);
      const m = /^([0-9a-f]+) blob (\d+)$/.exec(header);
      if (!m || m[1] !== oid) throw new Error(`git cat-file did not return blob ${oid}: ${JSON.stringify(header.slice(0, 120))}`);
      const end = eol + 1 + Number(m[2]);
      const body = out.subarray(eol + 1, end);
      at = end + 1;
      for (const { target, mode } of blobs.get(oid).targets) {
        fs.mkdirSync(path.dirname(target), { recursive: true });
        fs.writeFileSync(target, body, { mode });
      }
    }
  }
  return entries.map((entry) => entry.path);
}

/**
 * The tree a compile of `repo` would see, written under `dest`: HEAD plus every tracked and untracked, non-ignored file as it is
 * on disk, put through a throwaway index (the repository's own index is never touched; the blobs land in its object store).
 * Returns { tree, files }.
 */
export function exportWorktree(repo, dest) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-index-"));
  try {
    const env = gitEnv({ GIT_INDEX_FILE: path.join(scratch, "index") });
    const run = (args) => git(args, { cwd: repo, env, maxBuffer: 64 * 1024 * 1024 }).trim();
    run(["read-tree", "HEAD"]);
    run(["add", "-A"]);
    const tree = run(["write-tree"]);
    return { tree, files: exportCommit(repo, tree, dest) };
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
}

/** True when HEAD of `repo` is a branch that has no commit yet (just after `git init`, or an orphan branch before its first commit). */
export function hasNoCommit(repo) {
  const head = spawnSync("git", ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"], { cwd: repo, env: gitEnv(), encoding: "utf8" });
  if (head.error || head.status === 0) return false;
  const branch = spawnSync("git", ["symbolic-ref", "--quiet", "HEAD"], { cwd: repo, env: gitEnv(), encoding: "utf8" });
  return !branch.error && branch.status === 0;
}

/** The commit id `rev` names in `repo`. */
export function resolveCommit(repo, rev) {
  if (String(rev).startsWith("-")) throw new Error(`${JSON.stringify(rev)} is not a revision`);
  return git(["rev-parse", "--verify", `${rev}^{commit}`], { cwd: repo }).trim();
}

/** Run the selected rules over an exported tree. Returns { failures: [{ rule, gap, problem, fix, file?, subject?, identity? }], timings }. */
export function runRules({ root, commit, files, repo = process.cwd(), only = null, rules = RULES, now = new Date() }) {
  const failures = [];
  const timings = [];
  for (const rule of rules) {
    if (only && !only.includes(rule.id)) continue;
    const started = Date.now();
    let found;
    try { found = rule.run({ root, commit, files, repo, now }); }
    catch (error) { found = [{ problem: `the rule itself failed: ${error.message}` }]; }
    timings.push({ rule: rule.id, ms: Date.now() - started, failures: found.length });
    for (const f of found) failures.push({ rule: rule.id, gap: rule.gap, ...f, ...(f.file && f.subject !== undefined ? { identity: `${rule.id}|${f.file}|${f.subject}` } : {}) });
  }
  return { failures, timings };
}

// ---- the ratchet ------------------------------------------------------------------------------------------------------------------

export const APP_BASELINE_FILE = ".rightkit/app-check-baseline.json";
export const CENTRAL_BASELINE_FILE = "ci/app-check-baseline.json";

/** The identities RightKit's central baseline lists for `repoKey`; throws on a file that is not { known: { <key>: [string] } }. */
export function baselineKnown(text, repoKey) {
  if (text === null) return [];
  const known = JSON.parse(text)?.known;
  if (!known || typeof known !== "object" || Array.isArray(known) || !Object.values(known).every((v) => Array.isArray(v) && v.every((id) => typeof id === "string"))) throw new Error('expected { "known": { "<repo>": ["<identity>", ...] } }');
  return known[repoKey] ?? [];
}

/** The longest an app baseline entry may stay tolerated: its sunset is at most this many days after its own `added` date. */
export const APP_BASELINE_MAX_DAYS = 90;
/** The rule for an app baseline entry, as the messages state it. */
/** The sentence an expired entry's EXPIRED line and its finding's FAIL line both carry. */
const APP_BASELINE_EXPIRY_RULE = `an app baseline entry is tolerated for at most ${APP_BASELINE_MAX_DAYS} days after its added date`;
export const APP_BASELINE_RULE = `an app baseline entry needs a non-empty issue, an added date and a sunset date (YYYY-MM-DD, UTC); added is not later than today, and sunset is not before added and at most ${APP_BASELINE_MAX_DAYS} days after it`;

const DAY_MS = 24 * 60 * 60 * 1000;
/** `text` (YYYY-MM-DD) when it is a real calendar date, else null. */
const realDate = (text) => {
  if (typeof text !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(text)) return null;
  const date = new Date(`${text}T00:00:00Z`);
  return Number.isNaN(date.getTime()) || date.toISOString().slice(0, 10) !== text ? null : text;
};
/** The date `days` after `text` (YYYY-MM-DD, UTC), as YYYY-MM-DD. */
export const addDays = (text, days) => new Date(Date.parse(`${text}T00:00:00Z`) + days * DAY_MS).toISOString().slice(0, 10);

/**
 * An app's own baseline, { schema: 1, known: [{ id, issue, added, sunset }] }, as its entries; throws on any other shape or on an
 * entry that breaks APP_BASELINE_RULE. `today` (YYYY-MM-DD, UTC) is the latest an `added` date may be.
 */
export function appBaselineEntries(text, { today = new Date().toISOString().slice(0, 10) } = {}) {
  const data = JSON.parse(text);
  if (data?.schema !== 1 || !Array.isArray(data.known)) throw new Error('expected { "schema": 1, "known": [{ "id": "<identity>", "issue": "<repo#number>", "added": "YYYY-MM-DD", "sunset": "YYYY-MM-DD" }, ...] }');
  const seen = new Set();
  return data.known.map((entry, i) => {
    const at = `known[${i}]`;
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) throw new Error(`${at} must be an object with id, issue, added and sunset`);
    if (typeof entry.id !== "string" || !entry.id) throw new Error(`${at}.id must be a finding identity`);
    if (typeof entry.issue !== "string" || !entry.issue.trim()) throw new Error(`${at}.issue must name the issue that tracks ${entry.id}; ${APP_BASELINE_RULE}`);
    const added = realDate(entry.added);
    if (!added) throw new Error(`${at}.added must be the date the entry was added, YYYY-MM-DD; ${APP_BASELINE_RULE}`);
    if (added > today) throw new Error(`${at}.added ${added} is later than today (${today}, UTC); ${APP_BASELINE_RULE}`);
    const sunset = realDate(entry.sunset);
    if (!sunset) throw new Error(`${at}.sunset must be a date, YYYY-MM-DD; ${APP_BASELINE_RULE}`);
    if (sunset < added) throw new Error(`${at}.sunset ${sunset} is before added ${added}; ${APP_BASELINE_RULE}`);
    const latest = addDays(added, APP_BASELINE_MAX_DAYS);
    if (sunset > latest) throw new Error(`${at}.sunset ${sunset} is more than ${APP_BASELINE_MAX_DAYS} days after added ${added} (the latest is ${latest}); ${APP_BASELINE_RULE}`);
    if (seen.has(entry.id)) throw new Error(`${at} repeats ${entry.id}`);
    seen.add(entry.id);
    return { id: entry.id, issue: entry.issue, added, sunset };
  });
}

/** The ratchet: split findings into `fresh` (fail), `known` (listed: reported, not failing) and `stale` (a listed identity that no
 * finding has any more; fails, so the list only shrinks). With `rules` (--only) only those rules' entries can be stale. */
export function applyBaseline(findings, known, rules = null) {
  const listed = new Set(known);
  const present = new Set(findings.map((f) => f.identity).filter(Boolean));
  return {
    fresh: findings.filter((f) => !(f.identity && listed.has(f.identity))),
    known: findings.filter((f) => f.identity && listed.has(f.identity)),
    stale: [...listed].filter((id) => !present.has(id) && (!rules || rules.includes(id.split("|")[0]))).sort(),
  };
}

const sameDirectory = (a, b) => { try { return fs.realpathSync(a) === fs.realpathSync(b); } catch { return path.resolve(a) === path.resolve(b); } };

/** RightKit's own checkout when this module is loaded from one (packages/release/app-check.mjs), else null: an installed package has none. */
export function rightkitCheckout(here = HERE) {
  const root = path.resolve(here, "..", "..");
  const inPackages = path.basename(here) === "release" && path.basename(path.dirname(here)) === "packages";
  return inPackages && fs.existsSync(path.join(root, "scripts", "preflight.mjs")) && fs.existsSync(path.join(root, CENTRAL_BASELINE_FILE)) ? root : null;
}

/** The baseline for one run: { entries: [{ id, issue?, added?, sunset? }], listedIn }. `root` is the exported tree. Throws { file, message }. */
function loadBaseline({ explicit, root, central, repoKey, today }) {
  let file = explicit ? path.resolve(explicit) : null;
  try {
    let text = null;
    let fromApp = false;
    if (file) text = fs.readFileSync(file, "utf8");
    else if ((text = readText(root, APP_BASELINE_FILE)) !== null) { file = APP_BASELINE_FILE; fromApp = true; }
    else if (central) {
      file = path.join(central, CENTRAL_BASELINE_FILE);
      try { text = fs.readFileSync(file, "utf8"); } catch (e) { if (e.code !== "ENOENT") throw e; }
    }
    // The app's own file is always read as an app baseline (an issue, an added date and a sunset per entry, appBaselineEntries refuses
    // any other shape): sniffing its shape would let an app write the central { known: { "<repo>": [...] } } form, which carries
    // no dates, and silence a finding for good.
    if (fromApp || (text !== null && Array.isArray(JSON.parse(text)?.known))) return { entries: appBaselineEntries(text, { today }), listedIn: `${APP_BASELINE_FILE} known`, listedFile: APP_BASELINE_FILE };
    return { entries: baselineKnown(text, repoKey).map((id) => ({ id })), listedIn: `${CENTRAL_BASELINE_FILE} known["${repoKey}"]`, listedFile: CENTRAL_BASELINE_FILE };
  } catch (error) { throw Object.assign(new Error(error.message), { file }); }
}

// ---- the check ---------------------------------------------------------------------------------------------------------------------

/**
 * Check one repository. `mode` is "commit" (the committed files of `commit`, default HEAD) or "worktree" (what a compile would
 * see). Returns { findings (failing), known (tolerated by the baseline), stale (identities), expired ([{ id, issue, added, sunset }]),
 * found (every finding), exitCode, lines, text (stdout), errLines, errText (stderr) }; nothing is printed.
 * `rules` is the rule list (this module's, or RightKit's preflight adds its own), `label` prefixes the summary, `central` is the
 * RightKit checkout whose ci/app-check-baseline.json is the last baseline resort (null: none), `now` dates the sunsets.
 * `refuse` lists rules whose findings the baseline cannot excuse here, because the build refuses them whatever the baseline says
 * (BUILD_REFUSES); a listed one fails like a new one.
 */
export function runAppCheck({ repo, mode = "commit", commit, only = null, baseline, printBaseline = false, rules = RULES, label = "check", repoKey, central = rightkitCheckout(), now = new Date(), refuse = [] } = {}) {
  const lines = [];
  const errLines = [];
  const finish = (exitCode, extra = {}) => ({ findings: [], known: [], stale: [], expired: [], found: [], ...extra, exitCode, lines, text: lines.join("\n"), errLines, errText: errLines.join("\n") });
  const unknown = only?.filter((id) => !rules.some((r) => r.id === id)) ?? [];
  if (unknown.length) { errLines.push(`${label}: unknown rule(s): ${unknown.join(", ")} (see --list)`); return finish(2); }
  if (mode !== "commit" && mode !== "worktree") { errLines.push(`${label}: mode must be commit or worktree, not ${JSON.stringify(mode)}`); return finish(2); }
  if (mode === "worktree" && commit) { errLines.push(`${label}: --worktree and --commit are exclusive`); return finish(2); }
  repo = path.resolve(repo ?? process.cwd());
  // The baseline is keyed by the checked repository's name; a RightKit checkout is "rightkit" wherever it was cloned.
  const key = repoKey ?? (central && sameDirectory(repo, central) ? "rightkit" : path.basename(repo).toLowerCase());
  const started = Date.now();
  let checked, files, root;
  try {
    root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-"));
    if (mode === "worktree") ({ tree: checked, files } = exportWorktree(repo, root));
    else { checked = resolveCommit(repo, commit ?? "HEAD"); files = exportCommit(repo, checked, root); }
  } catch (error) {
    // HEAD is what both modes start from: a repository that has no commit yet has nothing to read, and "commit first" is the fix.
    const unborn = (mode === "worktree" || (commit ?? "HEAD") === "HEAD") && hasNoCommit(repo);
    errLines.push(unborn ? `${label}: the repository has no commit yet; commit first` : `${label}: could not export the commit to check: ${error.message}`);
    if (root) fs.rmSync(root, { recursive: true, force: true });
    return finish(2);
  }
  try {
    const { failures: found } = runRules({ root, commit: checked, files, repo, only, rules, now });
    if (printBaseline) {
      lines.push(JSON.stringify([...new Set(found.map((f) => f.identity).filter(Boolean))].sort(), null, 2));
      const unkeyed = found.filter((f) => !f.identity).length;
      if (unkeyed) errLines.push(`${label}: ${unkeyed} finding(s) of rules without an identity are not baselineable and are not listed`);
      return finish(0, { found });
    }
    // An entry past its sunset is no longer tolerated: its finding fails like any other, and the entry itself is reported.
    const today = now.toISOString().slice(0, 10);
    let listed;
    try { listed = loadBaseline({ explicit: baseline, root, central, repoKey: key, today }); }
    catch (error) { errLines.push(`${label}: cannot read the baseline ${error.file}: ${error.message}`); return finish(2); }
    const ran = (id) => !only || only.includes(id.split("|")[0]);
    const pastSunset = (entry) => entry.sunset !== undefined && entry.sunset < today;
    const expired = listed.entries.filter((entry) => pastSunset(entry) && ran(entry.id));
    const applied = applyBaseline(found, listed.entries.filter((entry) => !pastSunset(entry)).map((entry) => entry.id), only);
    const refused = applied.known.filter((f) => refuse.includes(f.rule)).map((f) => ({ ...f, problem: `${f.problem} (the baseline lists it, but the build refuses it)` }));
    const lapsed = new Map(expired.map((entry) => [entry.id, entry]));
    const fresh = [...applied.fresh.map((f) => (f.identity && lapsed.has(f.identity) ? { ...f, problem: `${f.problem} (its baseline entry expired: sunset ${lapsed.get(f.identity).sunset}; ${APP_BASELINE_EXPIRY_RULE})` } : f)), ...refused];
    const known = applied.known.filter((f) => !refuse.includes(f.rule));
    const { stale } = applied;
    for (const f of fresh) {
      lines.push(`FAIL ${f.rule} (${f.gap}): ${f.problem}`);
      if (f.fix) lines.push(`     fix: ${f.fix}`);
    }
    for (const f of known) lines.push(`KNOWN ${f.rule}: ${f.problem}`);
    for (const id of stale) lines.push(`STALE baseline entry ${id}: no longer found in ${key}; remove it from ${listed.listedIn}`);
    for (const e of expired) lines.push(`EXPIRED baseline entry ${e.id}: sunset ${e.sunset} has passed (${e.issue}); ${APP_BASELINE_EXPIRY_RULE}${e.added ? ` (${e.added})` : ""}; fix the finding and remove the entry from ${listed.listedIn}`);
    const ruleCount = only ? only.length : rules.length;
    const seconds = ((Date.now() - started) / 1000).toFixed(1);
    const problems = fresh.length + stale.length + expired.length;
    const result = { findings: fresh, known, stale, expired, found };
    if (problems) {
      lines.push(`${label}: ${problems} problem(s) in ${checked.slice(0, 12)} (${ruleCount} rules, ${seconds}s${known.length ? `, ${known.length} known` : ""}). Each would refuse later in CI or publication; fix and commit, then push.`);
      return finish(1, result);
    }
    lines.push(`${label}: ${checked.slice(0, 12)} passed ${ruleCount} rules in ${seconds}s${known.length ? ` (${known.length} known finding(s) in ${listed.listedFile}; fix them, then remove their entries)` : ""}`);
    return finish(0, result);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

// ---- which right-release commands run the check ------------------------------------------------------------------------------------

/** Commands that compile, package, sign, ship or inspect a release run the check first, in-process, and start nothing when it is red.
 * "commit" checks HEAD (what is built, packaged, signed and uploaded); "worktree" checks what a compile or the doctor would see
 * (the commands a person runs against the tree on disk: dev, doctor, test, stage-sidecar, sign-dev-artifact). There is no flag that
 * skips it. Each was found by reading the command's module and its callers (a compile, a package or a signing step that a person or an
 * agent can start from the CLI): test runs the app's suites (cargo, vitest) so it is dev-like; web-build is `build --web` and
 * checks as it does; swift-package packages and signs; recipe-build runs the Tauri build of a package recipe; stage-sidecar builds
 * the sidecar crates through the broker (apps run it from their own scripts, so it is dev-like); sign-candidate signs;
 * sign-dev-artifact signs a CI dev artifact from the checked-out repository (the dev-mac-sign and sign-existing jobs run it there),
 * so it checks the tree on disk; `addon build` runs the add-on's own target.build.cmd (a compile), then signs and seals the output of
 * HEAD, so it checks HEAD. A key with a space names a subcommand: "addon build" guards `right-release addon build` and no other
 * `addon` form (see UNCHECKED_COMMANDS). */
export const CHECKED_COMMANDS = new Map([
  ["release", "commit"], ["build", "commit"], ["package", "commit"], ["dev", "worktree"], ["qa", "commit"], ["upload", "commit"],
  ["publish", "commit"], ["publish-update", "commit"], ["github", "commit"], ["doctor", "worktree"],
  ["sign-windows", "commit"], ["sign-updater", "commit"], ["create-mac-updater", "commit"],
  ["test", "worktree"], ["web-build", "commit"], ["swift-package", "commit"], ["recipe-build", "commit"], ["stage-sidecar", "worktree"], ["sign-candidate", "commit"],
  ["addon build", "commit"], ["sign-dev-artifact", "worktree"],
]);

/** The key of CHECKED_COMMANDS an invocation matches, or null: `<command> <subcommand>` when that pair is listed, else the command. */
export function checkedCommandOf(args) {
  const [first, second] = args;
  if (!first) return null;
  if (second && CHECKED_COMMANDS.has(`${first} ${second}`)) return `${first} ${second}`;
  return CHECKED_COMMANDS.has(first) ? first : null;
}

/** Rules whose findings a baseline cannot excuse before a build, doctor or release step: the build refuses them whatever the baseline
 * says (no switches inventory: the release-switches scan has nothing to prove against), so tolerating one only moves the failure
 * minutes into the build. */
export const BUILD_REFUSES = Object.freeze(["switches-inventory"]);

/** Every other right-release command, each with why the check does not guard it. A command that is in neither map fails the
 * classification test in app-check.test.mjs, so a new command cannot appear unclassified. */
export const UNCHECKED_COMMANDS = new Map([
  ["check", "is the check"],
  ["update", "runs @rightkit/outdated over the app's RightKit pins (right-update); reads no release inputs and builds nothing"],
  ["deps", "the shared dependency lane: reads and updates dependency pins, not the app's release inputs"],
  ["hardening", "scans artifacts named on the command line; reads no source tree"],
  ["harden", "alias of hardening"],
  ["hardeningscan", "alias of hardening"],
  ["sign-qa-bundle", "QA-only ad hoc signing inside a scratch root; not a release artifact"],
  ["engine-locate", "prints the verified path of an installed engine; reads no app source"],
  ["init", "renames the desktop template in place; there is no committed app to check yet"],
  ["pe", "inspects one PE file named on the command line"],
  ["icons", "packs PNG frames named on the command line"],
  ["signing-agent", "manages the host signing service; no app"],
  ["mirror-root-artifact", "copies artifacts a build already produced"],
  ["license-notices", "writes third-party notices; no release input"],
  ["toolchain", "installs a pinned toolchain on a runner; no app"],
  ["cargo-target", "prints the cargo target directory from an offline `cargo metadata --no-deps`; compiles nothing"],
  ["ownership", "the ownership scan is its own gate (pre-commit, admission)"],
  ["ownership-approval", "retired no-op"],
  ["ownership-report", "reports ownership across a workspace"],
  ["ownership-mandatory", "reports the mandatory owner set"],
  ["adoption-evidence", "validates a published manifest and an adoption record; a report"],
  ["bundle-scan", "scans bundles named on the command line"],
  ["check-release-dependencies", "reads dependency manifests; a report"],
  ["dist-scan", "scans a built web dist named on the command line"],
  ["identity-scan", "asserts the signature of a path named on the command line"],
  ["apple-profile-create", "provisioning-profile credentials; no app source"],
  ["ios-credentials", "stores or checks App Store Connect credentials; no app source"],
  ["hooks", "installs the git hooks, and the pre-push hook runs the check"],
  ["suite-doctor", "verifies the Right Suite contract tests; a report"],
  ["cache", "host build-cache maintenance"],
  ["disk", "host disk maintenance"],
  ["install-app", "installs a signed build made earlier"],
  ["dmg", "takes --app/--out paths, compiles nothing, has no repository"],
  ["apple", "renews a provisioning profile; no app source"],
  ["qualify", "qualifies RightKit crates; preflight and the hosted scans check RightKit"],
  ["inventory", "reports registry drift"],
  ["model", "promotes a runtime model artifact"],
  ["addon", "every form but `addon build` (which is checked) compiles nothing: doctor validates the config, upload ships only what `addon build` sealed at HEAD, adopt verifies a downloaded add-on against a lock"],
  ["lsclean", "host LaunchServices cleanup"],
  ["generate-dmg-background", "generates an image"],
]);

/**
 * The places in the source that show the facts an UNCHECKED_COMMANDS reason states, for the commands whose reason is about the
 * source and not "a report" or "host maintenance": [file under packages/release, text that file must contain]. app-check.test.mjs
 * reads every place, so a reason cannot outlive the code it names. dmg: it needs --app, --out and --volname on the command line, and
 * dmg-build.test.mjs pins that `right-release dmg build` dispatches from a directory that is no repository (guarding it would turn
 * that dispatch into "needs a git repository"). addon: the forms other than build; upload ships only a seal whose manifest commit is
 * HEAD, which only `addon build` writes, and adopt takes a lock and an output directory.
 */
export const UNCHECKED_PROOF = new Map([
  ["dmg", [
    ["dmg-build.mjs", '"--app, --out & --volname are required"'],
    ["dmg-build.test.mjs", "installed CLI dispatches dmg build"],
  ]],
  ["addon", [
    ["addon-command.mjs", "manifest.commit !== commit) throw new Error(\"sealed add-on identity mismatch\")"],
    ["addon-command.mjs", '"adopt requires --lock and --output"'],
    ["addon-command.mjs", "run(target.build.cmd, target.build.args, root);"],
  ]],
]);

/** The mode `right-release <args>` must pass the check in, or null for a command the check does not guard. */
export function checkModeFor(args) {
  const [first, second] = args;
  if (!first) return null; // right-release alone prints help
  if (first.startsWith("-")) return ["-h", "--help", "-v", "--version"].includes(first) ? null : "commit"; // direct flags run `build <flags>`
  if (first === "publish" && ["cargo", "npm", "swift"].includes(second)) return null; // publishing a RightKit unit, not an app
  if (first === "qa" && second === "audit") return null; // a report
  if (first === "doctor" && args.includes("--all")) return null; // the suite contract test, no app
  const key = checkedCommandOf(args);
  return key ? CHECKED_COMMANDS.get(key) : null;
}

/** The directory whose git top-level is the app: --config's folder when given, else --app-root (recipe-build runs its plan there),
 * else `cwd`. */
export const appDirectory = (args, cwd) => {
  const valueOf = (flag) => {
    const i = args.findIndex((a) => a === flag || a.startsWith(`${flag}=`));
    return i < 0 ? null : args[i] === flag ? args[i + 1] : args[i].slice(flag.length + 1);
  };
  const config = valueOf("--config");
  if (config) return path.dirname(path.resolve(cwd, config));
  const appRoot = valueOf("--app-root");
  return appRoot ? path.resolve(cwd, appRoot) : cwd;
};

/**
 * Run the check a `right-release <args>` invocation owes, in this process. Returns null when the command is not guarded or the
 * check is green (nothing is printed then), else { exitCode: 1, lines } for the caller to print: the findings and why nothing
 * was started. A check that cannot run is red too.
 */
export function guardCommand({ args, cwd = process.cwd(), run = runAppCheck } = {}) {
  const mode = checkModeFor(args);
  if (!mode) return null;
  const name = args[0].startsWith("-") ? "build" : checkedCommandOf(args) ?? args[0];
  const dir = appDirectory(args, cwd);
  const repo = repoRoot(dir);
  if (!repo) return { exitCode: 1, lines: [`right-release ${name}: the app check needs a git repository and ${dir} is not inside one; nothing was started`] };
  const result = run({ repo, mode, label: "check", refuse: BUILD_REFUSES });
  if (result.exitCode === 0) return null;
  const why = result.exitCode === 2 ? "could not run" : "found problems";
  return { exitCode: 1, lines: [...result.lines, ...result.errLines, `right-release ${name}: the app check ${why} in ${repo}; nothing was started. Fix the above and commit, then run it again.`] };
}

// ---- command line ---------------------------------------------------------------------------------------------------------------

/**
 * `check [--repo <dir>] [--commit <rev> | --worktree] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>]`.
 * `--repo` defaults to the git top-level of `cwd` (or `defaultRepo`). Returns the exit code: 0 pass, 1 findings, 2 could not run.
 */
export async function main(argv, { out = (s) => process.stdout.write(`${s}\n`), err = (s) => process.stderr.write(`${s}\n`), label = "check", rules = RULES, defaultRepo = null, cwd = process.cwd(), central, now } = {}) {
  const arg = (flag) => { const i = argv.indexOf(flag); return i >= 0 ? argv[i + 1] : undefined; };
  if (argv.includes("--list")) { for (const r of rules) out(`${r.id} (${r.gap})`); return 0; }
  const repo = arg("--repo") ? path.resolve(arg("--repo")) : defaultRepo ?? repoRoot(cwd);
  if (!repo) { err(`${label}: not inside a git repository (use --repo <dir>)`); return 2; }
  const result = runAppCheck({
    repo,
    mode: argv.includes("--worktree") ? "worktree" : "commit",
    commit: arg("--commit"),
    only: arg("--only")?.split(",").filter(Boolean) ?? null,
    baseline: arg("--baseline"),
    printBaseline: argv.includes("--print-baseline"),
    rules, label, ...(central === undefined ? {} : { central }), ...(now ? { now } : {}),
  });
  for (const line of result.lines) out(line);
  for (const line of result.errLines) err(line);
  return result.exitCode;
}
