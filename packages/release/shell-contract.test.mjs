import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { selectedContentDevtoolsCrate } from "./devtools-policy.mjs";
import { callArgs, contentDevtoolsCrate, discoverApps, matchClose, mergePatch, parseCsp, readJsxTag, rustFns, scanJs, scanRust, SHELL_CONTRACT_RULES, shellExports, shellMembers } from "./shell-contract.mjs";

const SRC = "src-tauri";
const CONF = `${SRC}/tauri.conf.json`;
const tree = (t, files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-shell-contract-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [file, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), typeof text === "string" ? text : JSON.stringify(text));
  }
  return { root, files: Object.keys(files) };
};
// An app is a tauri.conf.json: a fixture that names none gets an empty one at the usual place.
const run = (id, t, files, extra = {}) => SHELL_CONTRACT_RULES.find((r) => r.id === id).run({ ...tree(t, Object.keys(files).some((f) => f.endsWith("tauri.conf.json")) ? files : { [CONF]: JSON.stringify({ app: {} }), ...files }), ...extra });
const ids = (findings) => findings.map((f) => `${f.file}|${f.subject}`).sort();
const subjects = (findings) => findings.map((f) => f.subject).sort();

// A finding names its file and line first, has an identity that carries no line number, and says the change.
const wellFormed = (findings) => {
  for (const f of findings) {
    assert.equal(typeof f.file, "string");
    assert.equal(typeof f.subject, "string");
    assert.ok(f.problem.startsWith(`${f.file}:`) && /^[^:\s]+:\d+ /.test(f.problem), f.problem);
    assert.ok(f.fix && f.fix.length > 10, `${f.subject} has no fix`);
    assert.doesNotMatch(f.subject, /:\d+$/, "a line number in the subject changes the identity when code moves above it");
  }
  return findings;
};

const win = (extra = {}) => ({ label: "main", title: "App", ...extra });
const conf = (windows, security) => ({ productName: "App", app: { windows, ...(security === undefined ? {} : { security }) } });
const GOOD_CSP = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ipc: http://ipc.localhost; frame-ancestors 'none'";

test("the nine rules, in order, each with its gap id", () => {
  assert.deepEqual(SHELL_CONTRACT_RULES.map((r) => `${r.id}:${r.gap}`), [
    "shell-window-hidden:SC-R1", "shell-plugin-order:SC-R2", "shell-handler:SC-R3", "shell-csp:SC-R4", "shell-grants:SC-R5",
    "qa-window-label:SC-R6", "devtools-declaration:SC-R7", "shell-lifecycle-external:SC-R8", "legal-receipt-native:SC-R9",
  ]);
  assert.ok(Object.isFrozen(SHELL_CONTRACT_RULES));
});

// ---- helpers ---------------------------------------------------------------------------------------------------------------
test("mergePatch follows the RFC 7396 test vectors: objects merge, arrays and scalars replace, null deletes", () => {
  const vectors = [
    [{ a: "b" }, { a: "c" }, { a: "c" }],
    [{ a: "b" }, { b: "c" }, { a: "b", b: "c" }],
    [{ a: "b" }, { a: null }, {}],
    [{ a: "b", b: "c" }, { a: null }, { b: "c" }],
    [{ a: ["b"] }, { a: "c" }, { a: "c" }],
    [{ a: "c" }, { a: ["b"] }, { a: ["b"] }],
    [{ a: { b: "c" } }, { a: { b: "d", c: null } }, { a: { b: "d" } }],
    [{ a: [{ b: "c" }] }, { a: [1] }, { a: [1] }],
    [["a", "b"], ["c", "d"], ["c", "d"]],
    [{ a: "b" }, ["c"], ["c"]],
    [{ a: "foo" }, null, null],
    [{ a: "foo" }, "bar", "bar"],
    [{ e: null }, { a: 1 }, { e: null, a: 1 }],
    [[1, 2], { a: "b", c: null }, { a: "b" }],
    [{}, { a: { bb: { ccc: null } } }, { a: { bb: {} } }],
  ];
  for (const [target, patch, expected] of vectors) assert.deepEqual(mergePatch(target, patch), expected, JSON.stringify([target, patch]));
  const target = { a: { b: 1 } };
  mergePatch(target, { a: { b: 2 } });
  assert.deepEqual(target, { a: { b: 1 } }, "the target is not modified");
  assert.equal(mergePatch({}, JSON.parse('{"__proto__": {"x": 1}}')).x, undefined);
});

test("scanRust blanks comments, keeps offsets, keeps literal text in `code` and blanks it in `skel`", () => {
  const text = 'fn a() { // { not a block\n  let s = "}{ // not a comment"; /* ) */ let r = r#"a "quoted" ) "#; let c = \'{\'; let l: &\'static str = "x"; }\n';
  const { code, skel } = scanRust(text);
  assert.equal(code.length, text.length);
  assert.equal(skel.length, text.length);
  assert.equal(code.split("\n").length, text.split("\n").length);
  assert.ok(code.includes('"}{ // not a comment"') && code.includes('r#"a "quoted" ) "#'), "literals stay in code");
  assert.ok(!code.includes("not a block") && !code.includes("/* )"), "comments go from both");
  const open = skel.indexOf("{");
  assert.equal(skel[matchClose(skel, open)], "}");
  assert.equal(matchClose(skel, open), skel.lastIndexOf("}"), "no brace in a string, comment or char literal unbalances the skeleton");
  assert.deepEqual(rustFns(skel).map((f) => f.name), ["a"]);
});

test("scanRust drops #[cfg(test)] items but keeps a test-only match arm, so the braces stay balanced", () => {
  const source = 'fn phase(p: P) -> &str {\n  match p {\n    P::A => "a",\n    #[cfg(test)]\n    P::B => "b",\n    P::C => "c",\n  }\n}\n\n#[cfg(test)]\nmod tests {\n  use super::*;\n  #[test] fn t() { assert!(true); }\n}\n#[cfg(test)]\nfn only_in_tests() { x(); }\nfn kept() { plugin(); }\n';
  const { skel } = scanRust(source);
  assert.equal((skel.match(/{/g) ?? []).length, (skel.match(/}/g) ?? []).length);
  assert.ok(skel.includes("P::C") && skel.includes("fn kept"));
  assert.ok(!skel.includes("only_in_tests") && !skel.includes("mod tests"));
  assert.equal(scanRust("#![cfg(test)]\nfn a() { b(); }\n").skel.trim(), "");
});

test("callArgs splits at top-level commas only", () => {
  const { skel } = scanRust('f(a, g(b, c), [1, 2], { x: 1, y: 2 }, "s,t",)');
  const args = callArgs(skel, skel.indexOf("(")).map(([s, e]) => skel.slice(s, e));
  assert.equal(args.length, 5);
  assert.equal(args[1], "g(b, c)");
});

test("scanJs keeps strings and JSX text from unbalancing a tag; readJsxTag reads attributes, spreads and arrow functions", () => {
  const source = `const re = /["'(]/g; // ) in a comment\nconst a = <Onboarding legalMode="none" onComplete={() => finish({ ok: true })} requiredVersion='1' {...rest} dragRegion />;\nconst b = <p>Don't {x}</p>;\n`;
  const { code, skel } = scanJs(source);
  assert.equal(code.length, source.length);
  const tag = readJsxTag(skel, code, skel.indexOf("<Onboarding"));
  assert.equal(tag.spread, true);
  assert.deepEqual([...tag.attrs].map(([name, a]) => `${name}:${a.kind}`), ["legalMode:string", "onComplete:expr", "requiredVersion:string", "dragRegion:true"]);
  assert.equal(tag.attrs.get("onComplete").text, "() => finish({ ok: true })");
  assert.equal(source.slice(tag.end - 2, tag.end), "/>");
  assert.equal((skel.match(/{/g) ?? []).length, (skel.match(/}/g) ?? []).length);
  // `} />` after an attribute is the end of a tag, not a regular expression
  const closing = scanJs('<A b={1} /><C d={f("x")} />;\n');
  assert.equal(closing.skel, '<A b={1} /><C d={f(" ")} />;\n', "only the string is blanked; the second /> is not the end of a regular expression");
});

test("parseCsp reads a string and a directive object, and reports no policy as null", () => {
  assert.deepEqual([...parseCsp("default-src 'self'; Script-Src 'self' 'unsafe-inline'; ")], [["default-src", ["'self'"]], ["script-src", ["'self'", "'unsafe-inline'"]]]);
  assert.deepEqual([...parseCsp({ "default-src": "'self'", "connect-src": ["'self'", "https://a.example"] })], [["default-src", ["'self'"]], ["connect-src", ["'self'", "https://a.example"]]]);
  assert.equal(parseCsp(""), null);
  assert.equal(parseCsp({}), null);
  assert.equal(parseCsp(undefined), null);
  assert.equal(parseCsp(["default-src 'self'"]), null);
});

test("discoverApps: one app per tracked tauri.conf.json outside node_modules, vendor and target; roots, release config and nested apps", (t) => {
  const { root, files } = tree(t, {
    "right-release.config.mjs": "export default { app: 'root' };\n",
    "package.json": "{}",
    "src-tauri/tauri.conf.json": conf([]),
    "src-tauri/tauri.macos.conf.json": {},
    "src-tauri/tauri.dev.conf.json": {},
    "src-tauri/capabilities/default.json": { permissions: [] },
    "src-tauri/src/lib.rs": "fn a() {}\n",
    "src-tauri/src/lib_tests.rs": "fn t() {}\n",
    "src-tauri/tests/it.rs": "fn t() {}\n",
    "src-tauri/target/debug/x.rs": "fn t() {}\n",
    "src/App.tsx": "export {}\n",
    "src/App.test.tsx": "export {}\n",
    "scripts/qa.mjs": "export {}\n",
    "node_modules/pkg/tauri.conf.json": "{}",
    "vendor/x/tauri.conf.json": "{}",
    "apps/other/right-release.config.json": '{ "app": "other" }',
    "apps/other/tauri.conf.json": conf([]),
    "apps/other/src/main.ts": "export {}\n",
    "apps/other/src/main.rs": "fn main() {}\n",
  });
  const apps = discoverApps({ root, files });
  assert.deepEqual(apps.map((a) => a.confDir), ["apps/other", "src-tauri"]);
  const [other, main] = apps;
  assert.equal(main.appRoot, "", "src-tauri's parent is the app root");
  assert.equal(other.appRoot, "apps/other", "a conf outside src-tauri is its own root");
  assert.equal(main.releaseConfig, "right-release.config.mjs");
  assert.equal(other.releaseConfig, "apps/other/right-release.config.json");
  assert.deepEqual(main.platforms.map((p) => p.file), ["src-tauri/tauri.macos.conf.json"], "tauri.dev.conf.json is no platform patch");
  assert.deepEqual(main.rustFiles, ["src-tauri/src/lib.rs"], "tests, *_tests.rs and target are skipped");
  assert.deepEqual(main.rendererFiles, ["right-release.config.mjs", "src/App.tsx"], "tests, scripts, src-tauri and the nested app's files are not this app's renderer");
  assert.deepEqual(other.rendererFiles, ["apps/other/src/main.ts"]);
  assert.deepEqual(main.capabilityFiles, ["src-tauri/capabilities/default.json"]);
});

// ---- R1 shell-window-hidden --------------------------------------------------------------------------------------------------
test("R1 shell-window-hidden: a window created visible fails, a hidden one passes", (t) => {
  const [finding, ...rest] = wellFormed(run("shell-window-hidden", t, { [CONF]: `{\n  "app": {\n    "windows": [\n      { "label": "main", "title": "App" }\n    ]\n  }\n}\n` }));
  assert.equal(rest.length, 0);
  assert.deepEqual(ids([finding]), [`${CONF}|base:main`]);
  assert.match(finding.problem, /^src-tauri\/tauri\.conf\.json:4 window "main" is created at startup on base without "visible": false/);
  assert.match(finding.fix, /set "visible": false/);
  assert.deepEqual(run("shell-window-hidden", t, { [CONF]: conf([win({ visible: false })]) }), []);
  assert.deepEqual(subjects(run("shell-window-hidden", t, { [CONF]: conf([win({ visible: true })]) })), ["base:main"], "visible: true is not hidden");
  assert.deepEqual(subjects(run("shell-window-hidden", t, { [CONF]: conf([{ title: "no label" }]) })), ["base:main"], "no label means main");
});

test("R1: a macOS patch that restates windows without visible:false fails on macos only; Tauri replaces the array", (t) => {
  const files = {
    [CONF]: conf([win({ visible: false })]),
    [`${SRC}/tauri.macos.conf.json`]: { app: { windows: [win({ titleBarStyle: "Overlay" })] } },
    [`${SRC}/tauri.windows.conf.json`]: { bundle: { targets: ["nsis"] } },
    [`${SRC}/tauri.linux.conf.json`]: { app: { windows: [win({ visible: false })] } },
  };
  const findings = wellFormed(run("shell-window-hidden", t, files));
  assert.deepEqual(ids(findings), [`${SRC}/tauri.macos.conf.json|macos:main`]);
  assert.match(findings[0].problem, /restates app\.windows, and Tauri replaces arrays on merge/);
  // a platform file that does not restate windows inherits the base result and adds no finding of its own
  const inherited = run("shell-window-hidden", t, { ...files, [`${SRC}/tauri.macos.conf.json`]: { bundle: {} }, [CONF]: conf([win()]) });
  assert.deepEqual(ids(inherited), [`${CONF}|base:main`]);
});

test("R1: windows that are not created at startup, and mobile targets, are not judged", (t) => {
  assert.deepEqual(run("shell-window-hidden", t, { [CONF]: conf([win({ create: false })]) }), [], "create: false is built in code");
  const mobile = { [CONF]: conf([win({ visible: false })]), [`${SRC}/tauri.ios.conf.json`]: { app: { windows: [win({ visible: true })] } } };
  assert.deepEqual(run("shell-window-hidden", t, mobile), [], "the shell is a desktop plugin");
  assert.deepEqual(run("shell-window-hidden", t, { [CONF]: conf([]) }), []);
  assert.deepEqual(run("shell-window-hidden", t, { [CONF]: { app: {} } }), []);
});

test("R1: a malformed tauri.conf.json is a finding with an identity, not an exception", (t) => {
  const findings = wellFormed(run("shell-window-hidden", t, { [CONF]: "{ not json", [`${SRC}/tauri.macos.conf.json`]: "[" }));
  assert.deepEqual(ids(findings), [`${CONF}|base:unparseable`, `${SRC}/tauri.macos.conf.json|macos:unparseable`]);
  assert.deepEqual(run("shell-window-hidden", t, { [CONF]: "null", [`${SRC}/tauri.windows.conf.json`]: "7" }), []);
});

// ---- R2 shell-plugin-order ---------------------------------------------------------------------------------------------------
const SHELL_FN = `use rightkit_tauri_shell::{Shell, ShellPlugin};

/// The shared shell, lifecycle-only.
fn shell_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let shell = Shell::builder("heardright").app_name("HeardRight").main_label("hub").build();
    ShellPlugin::builder().shell(shell).lifecycle_only()
}
`;
const RUN_FN = (chain) => `pub fn run() {\n    let mut builder = ${chain};\n    builder.run(tauri::generate_context!()).unwrap();\n}\n`;

test("R2 shell-plugin-order: CutRight's setup-time install fails", (t) => {
  const source = `pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let shell = rightkit_tauri_shell::Shell::builder("cutright").app_name("CutRight").build();
            app.handle().plugin(shell.plugin())?;
            Ok(())
        })
        .build(tauri::generate_context!());
}
`;
  const findings = wellFormed(run("shell-plugin-order", t, { [`${SRC}/src/lib.rs`]: source }));
  assert.deepEqual(ids(findings), [`${SRC}/src/lib.rs|setup-install`], "one finding: the shell is not on the builder chain, so nothing is 'before' it");
  assert.match(findings[0].problem, /^src-tauri\/src\/lib\.rs:6 installs the shell plugin from setup/);
});

test("R2: HeardRight's helper-function shape passes while the shell plugin is first, and fails behind another plugin", (t) => {
  const ok = `${SHELL_FN}${RUN_FN("tauri::Builder::default().plugin(shell_plugin())").replace("builder.run", "builder.plugin(tauri_plugin_dialog::init()).plugin(tauri_plugin_os::init()).run")}`;
  assert.deepEqual(run("shell-plugin-order", t, { [`${SRC}/src/app_run.rs`]: ok }), []);
  const late = `${SHELL_FN}${RUN_FN("tauri::Builder::default().plugin(tauri_plugin_dialog::init()).plugin(shell_plugin())")}`;
  const findings = wellFormed(run("shell-plugin-order", t, { [`${SRC}/src/app_run.rs`]: late }));
  assert.deepEqual(subjects(findings), ["before:tauri_plugin_dialog::init"]);
  assert.match(findings[0].problem, /^src-tauri\/src\/app_run\.rs:\d+ registers tauri_plugin_dialog::init before the shell plugin/);
});

test("R2: a shell helper in another module is found by its module name, and a same-named function elsewhere is not a shell", (t) => {
  const files = {
    [`${SRC}/src/shell_plugin.rs`]: 'pub(crate) fn plugin() -> TauriPlugin<Wry> {\n    ShellPlugin::builder().shell(Shell::builder("mail").build()).without(Module::Watchdog).build()\n}\n',
    [`${SRC}/src/demo.rs`]: "pub fn plugin() -> TauriPlugin<Wry> { tauri::plugin::Builder::new(\"demo\").build() }\n",
    [`${SRC}/src/tauri_run.rs`]: RUN_FN("tauri::Builder::default().plugin(shell_plugin::plugin())"),
  };
  assert.deepEqual(run("shell-plugin-order", t, files), []);
  const late = { ...files, [`${SRC}/src/tauri_run.rs`]: RUN_FN("tauri::Builder::default().plugin(demo::plugin()).plugin(shell_plugin::plugin())") };
  assert.deepEqual(subjects(run("shell-plugin-order", t, late)), ["before:demo::plugin"], "demo::plugin is not a shell");
  const wrapped = { ...files, [`${SRC}/src/tauri_run.rs`]: "fn shell() -> TauriPlugin<Wry> { shell_plugin::plugin() }\n" + RUN_FN("tauri::Builder::default().plugin(tauri_plugin_os::init()).plugin(shell())") };
  assert.deepEqual(subjects(run("shell-plugin-order", t, wrapped)), ["before:tauri_plugin_os::init"], "a wrapper that calls a shell helper is a shell helper");
});

test("R2: a shell kept in a variable (ViewRight) is the shell plugin; QA plugins and the shell's own single-instance plugin may come first", (t) => {
  const body = (before) => `pub fn run() {
    let mut builder = tauri::Builder::default();
    let mut shell = rightkit_tauri_shell::Shell::builder("viewright").app_name("ViewRight");
    shell = shell.single_instance(true);
${before}    builder = builder.plugin(shell.build().lifecycle_plugin());
    builder = builder.plugin(external_navigation::plugin());
}
`;
  assert.deepEqual(run("shell-plugin-order", t, { [`${SRC}/src/lib.rs`]: body("") }), []);
  const qa = `    if let Some(control) = rightkit_qa::embedded::Control::<tauri::Wry>::new().build_if_enabled() {
        builder = builder.plugin(control);
    }
    if let Some(plugin) = control_builder.build_if_enabled() {
        builder = builder.plugin(plugin);
    }
    builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder = builder.plugin(rightkit_tauri_shell::Shell::single_instance_plugin(|_app, _argv, _cwd| {}));
`;
  assert.deepEqual(run("shell-plugin-order", t, { [`${SRC}/src/lib.rs`]: body(qa) }), [], "QA-only and single-instance plugins are allowed before the shell");
  const findings = run("shell-plugin-order", t, { [`${SRC}/src/lib.rs`]: body("    builder = builder.plugin(tauri_plugin_updater::Builder::new().build());\n") });
  assert.deepEqual(subjects(findings), ["before:tauri_plugin_updater::Builder::new"]);
});

test("R2: comments, test modules and files with no shell are not judged", (t) => {
  const source = `// builder.plugin(a).plugin(shell_plugin())\n${SHELL_FN}${RUN_FN("tauri::Builder::default().plugin(shell_plugin())")}#[cfg(test)]\nmod tests {\n    fn t() { tauri::Builder::default().plugin(x()).plugin(shell_plugin()); }\n}\n`;
  assert.deepEqual(run("shell-plugin-order", t, { [`${SRC}/src/a.rs`]: source, [`${SRC}/src/b.rs`]: RUN_FN("tauri::Builder::default().plugin(tauri_plugin_os::init())") }), []);
});

// ---- R3 shell-handler --------------------------------------------------------------------------------------------------------
const FAKE_INDEX = `
export const SURFACE_COMMAND = 'rk_shell_set_surface';
export const EVENTS = { quitRequested: 'rightkit-shell://quit-requested' } as const;
export function asShellError(e: unknown) { return { code: 'x', message: String(e) }; }
export function quitNow(invoke: Invoke) { return invoke('rk_shell_quit'); }
export function createShell(t: Transport) {
  const api = {
    a: () => invoke('rk_shell_open_external'), b: () => invoke('rk_shell_window'), c: () => invoke('rk_shell_quit'), d: () => invoke('rk_shell_status'),
    e: () => invoke('rk_shell_ready'), f: () => invoke('rk_shell_notify'), g: () => invoke('rk_shell_badge'), h: () => invoke('rk_shell_set_tray'),
    i: () => invoke('rk_shell_autostart'), j: () => invoke('rk_shell_set_theme'),
  };
  return api;
}
`;
const RS_LIST = (...cmds) => `pub fn run() {\n    tauri::Builder::default().invoke_handler(tauri::generate_handler![\n        my_command,\n${cmds.map((c) => `        rightkit_tauri_shell::commands::${c},\n`).join("")}    ]);\n}\n`;
const RS_HANDLER = "pub fn run() {\n    tauri::Builder::default().invoke_handler(rightkit_tauri_shell::handler![my_command]);\n}\n";

test("R3 shell-handler: a command the renderer invokes that generate_handler! does not list is missing; handler! registers all", (t) => {
  const renderer = { "src/shell.ts": "import { invoke } from '@tauri-apps/api/core';\nexport const close = () => invoke('rk_shell_window', { action: 'close' });\nexport const quit = () => invoke(\"rk_shell_quit\");\n" };
  const findings = wellFormed(run("shell-handler", t, { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_LIST("rk_shell_window"), ...renderer }, { shellIndex: FAKE_INDEX }));
  assert.deepEqual(ids(findings), [`${SRC}/src/lib.rs|missing:rk_shell_quit`], "a hand-listed subset is allowed; only the missing command is reported");
  assert.match(findings[0].problem, /^src-tauri\/src\/lib\.rs:2 does not register rk_shell_quit, which the renderer invokes \(src\/shell\.ts:3\)/);
  assert.deepEqual(run("shell-handler", t, { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_HANDLER, ...renderer }, { shellIndex: FAKE_INDEX }), []);
  assert.deepEqual(run("shell-handler", t, { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_LIST("rk_shell_window", "rk_shell_quit"), ...renderer }, { shellIndex: FAKE_INDEX }), []);
  const wrapped = "macro_rules! app_handler {\n    ($($extra:path),* $(,)?) => { rightkit_tauri_shell::handler![$($extra,)* my_command] };\n}\n";
  assert.deepEqual(run("shell-handler", t, { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: wrapped, ...renderer }, { shellIndex: FAKE_INDEX }), [], "handler! inside a wrapper macro still registers the shell");
});

test("R3: an import from @rightkit/tauri-shell is mapped to the commands in that function's body; a factory that wraps them all needs at least one", (t) => {
  const files = (imports, rust = RS_LIST("rk_shell_window")) => ({ [CONF]: conf([]), [`${SRC}/src/lib.rs`]: rust, "src/shell.ts": `import { ${imports} } from "@rightkit/tauri-shell";\nexport {};\n` });
  const mapped = run("shell-handler", t, files("quitNow, asShellError, EVENTS, type Shell"), { shellIndex: FAKE_INDEX });
  assert.deepEqual(subjects(mapped), ["missing:rk_shell_quit"], "quitNow's body calls rk_shell_quit; asShellError and EVENTS call nothing; a type import needs nothing");
  assert.match(mapped[0].problem, /\(src\/shell\.ts:1\)/);
  assert.deepEqual(subjects(run("shell-handler", t, files("SURFACE_COMMAND"), { shellIndex: FAKE_INDEX })), ["missing:rk_shell_set_surface"], "a constant maps to the literal it holds");
  assert.deepEqual(run("shell-handler", t, files("createShell"), { shellIndex: FAKE_INDEX }), [], "createShell wraps ten commands: the app lists the ones it uses, and one is enough");
  assert.deepEqual(subjects(run("shell-handler", t, files("createShell", RS_LIST()), { shellIndex: FAKE_INDEX })), ["missing:any"], "no shell command registered at all");
  assert.deepEqual(subjects(run("shell-handler", t, files("somethingNew", RS_LIST()), { shellIndex: FAKE_INDEX })), ["missing:any"], "an import the bridge map cannot place needs at least one");
  assert.deepEqual(run("shell-handler", t, files("createShell", RS_HANDLER), { shellIndex: FAKE_INDEX }), []);
  assert.deepEqual(run("shell-handler", t, { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_LIST(), "src/shell.ts": 'import type { Shell } from "@rightkit/tauri-shell";\nimport * as shell from "@rightkit/tauri-shell";\n' }, { shellIndex: FAKE_INDEX }).length, 1, "a namespace import reaches every command");
  assert.deepEqual(run("shell-handler", t, { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_LIST(), "src/shell.ts": 'import type { Shell } from "@rightkit/tauri-shell";\n' }, { shellIndex: FAKE_INDEX }), []);
});

test("R3: this checkout's bridge map is read from packages/tauri-shell/src/index.ts, and its names are not found elsewhere", (t) => {
  const exportsMap = shellExports(fs.readFileSync(new URL("../tauri-shell/src/index.ts", import.meta.url), "utf8"));
  assert.ok(exportsMap.get("createShell").length > 8, "createShell wraps the whole command set");
  assert.deepEqual(exportsMap.get("SURFACE_COMMAND"), ["rk_shell_set_surface"]);
  assert.deepEqual(exportsMap.get("tauriTransport"), []);
  assert.deepEqual(exportsMap.get("EVENTS"), []);
  const files = { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_LIST("rk_shell_window"), "src/shell.ts": 'import { createShell, EVENTS } from "@rightkit/tauri-shell";\n' };
  assert.deepEqual(run("shell-handler", t, files), [], "with the real map: createShell needs one command, and one is listed");
});

test("R3: createShell's members are mapped from the bridge source, and an app that reaches window.close() needs rk_shell_window", () => {
  const real = shellMembers(fs.readFileSync(new URL("../tauri-shell/src/index.ts", import.meta.url), "utf8"));
  assert.deepEqual(real.get("window.minimize"), ["rk_shell_window"]);
  assert.deepEqual(real.get("quit"), ["rk_shell_quit"]);
  assert.deepEqual(real.get("engine.status"), ["rk_shell_engine_status"]);
  assert.deepEqual(real.get("engine.onEvent"), [], "a listener calls no command");
  assert.deepEqual(real.get("surface.set"), ["rk_shell_set_surface"], "a command named through an exported constant");
  assert.deepEqual([...real.get("bridge")].sort(), ["rk_shell_open_external", "rk_shell_set_theme", "rk_shell_window"], "bridge() reaches window and link members through api.*");
  assert.ok(real.size > 60);
  assert.equal(shellMembers("export const x = 1;").size, 0);
  assert.equal(shellMembers(undefined).size, 0);
});

test("R3: what the renderer calls on its createShell() is checked against the hand-listed commands (CutRight's and CodeRight's shapes)", (t) => {
  const cutright = 'import { createShell } from "@rightkit/tauri-shell";\nexport const shell = createShell();\nexport const shellBridge = { window: { minimize: () => shell.window.minimize(), close: () => shell.window.close() } };\nexport const quitNow = () => shell.quit({ force: true });\n';
  const engine = 'import { shell } from "./shell";\nexport const status = () => shell.engine.status();\nexport const call = <T,>(m: string) => shell.engine.callTagged<T>(m);\nexport const on = (h) => shell.engine.onEvent(h);\n';
  const files = (rust) => ({ [CONF]: conf([]), [`${SRC}/src/lib.rs`]: rust, "src/lib/shell.ts": cutright, "src/lib/engine.ts": engine });
  const all = RS_LIST("rk_shell_window", "rk_shell_quit", "rk_shell_engine_status", "rk_shell_engine_call_tagged");
  assert.deepEqual(run("shell-handler", t, files(all)), [], "every member the app reaches is listed");
  const missing = wellFormed(run("shell-handler", t, files(RS_LIST("rk_shell_window", "rk_shell_quit"))));
  assert.deepEqual(subjects(missing), ["missing:rk_shell_engine_call_tagged", "missing:rk_shell_engine_status"]);
  assert.match(missing[0].problem, /which the renderer invokes \(src\/lib\/engine\.ts:2\)/);
  assert.deepEqual(subjects(run("shell-handler", t, files(RS_LIST("rk_shell_quit", "rk_shell_engine_status", "rk_shell_engine_call_tagged")))), ["missing:rk_shell_window"], "window.minimize and window.close are rk_shell_window");
  const coderight = 'import { createShell, type WindowAction } from "@rightkit/tauri-shell";\nexport function shellWindowAction(action: WindowAction): Promise<void> {\n  const shell = createShell({ invoke, listen });\n  return shell.window.do(action);\n}\n';
  const local = { [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_LIST("rk_shell_notify"), "src/lib/shellWindow.ts": coderight };
  assert.deepEqual(subjects(run("shell-handler", t, local)), ["missing:rk_shell_window"], "a shell made inside a function is followed by its variable");
  assert.deepEqual(run("shell-handler", t, { ...local, [`${SRC}/src/lib.rs`]: RS_LIST("rk_shell_window") }), []);
});

test("R3: a createShell() that is chained, passed on or used through bridge() is followed as far as it can be", (t) => {
  const app = (renderer, rust = RS_LIST("rk_shell_status")) => ({ [CONF]: conf([]), [`${SRC}/src/lib.rs`]: rust, "src/a.ts": `import { createShell } from "@rightkit/tauri-shell";\n${renderer}\n` });
  assert.deepEqual(subjects(run("shell-handler", t, app("createShell().window.close();"))), ["missing:rk_shell_window"], "a chained call");
  assert.deepEqual(run("shell-handler", t, app("createShell().window.close();", RS_LIST("rk_shell_window"))), []);
  assert.deepEqual(subjects(run("shell-handler", t, app("const bridge = createShell().bridge();"))), ["missing:rk_shell_open_external", "missing:rk_shell_set_theme", "missing:rk_shell_window"], "bridge() reaches window, link and theme commands");
  assert.deepEqual(run("shell-handler", t, app("const s = createShell();\nregister(s);")), [], "a shell passed on cannot be followed; one listed command is enough");
  assert.deepEqual(subjects(run("shell-handler", t, app("const s = createShell();\nregister(s);", RS_LIST()))), ["missing:any"]);
  assert.deepEqual(run("shell-handler", t, app("const s = createShell();\ns.mystery.call();")), [], "a member the bridge map does not know needs at least one");
  assert.deepEqual(subjects(run("shell-handler", t, app("const s = createShell();\ns.mystery.call();", RS_LIST()))), ["missing:any"]);
  assert.deepEqual(subjects(run("shell-handler", t, app("const shell = createShell();\nconst x = { shell: 1 };\ntype T = typeof shell;\nshell.status();", RS_LIST("rk_shell_window")))), ["missing:rk_shell_status"], "an object key and typeof are not uses; shell.status() is");
  assert.deepEqual(subjects(run("shell-handler", t, app("const s = createShell();"))), [], "a shell nothing is called on: one listed command is enough");
});

test("R3: renderer test and script files are not the shipped renderer; commented-out calls are not calls", (t) => {
  const files = {
    [CONF]: conf([]), [`${SRC}/src/lib.rs`]: RS_LIST(),
    "src/shell.test.ts": "invoke('rk_shell_quit');\n", "scripts/qa.mjs": "invoke('rk_shell_quit');\n", "src/a.ts": "// invoke('rk_shell_quit')\n/* 'rk_shell_window' */\n",
  };
  assert.deepEqual(run("shell-handler", t, files, { shellIndex: FAKE_INDEX }), []);
});

// ---- R4 shell-csp ------------------------------------------------------------------------------------------------------------
test("R4 shell-csp: a CSP without frame-ancestors fails (CodeRight's) and the full baseline passes", (t) => {
  const coderight = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' https://coderight.cc ws://127.0.0.1:*";
  const findings = wellFormed(run("shell-csp", t, { [CONF]: `{\n  "app": {\n    "security": {\n      "csp": "${coderight}"\n    }\n  }\n}\n` }));
  assert.deepEqual(ids(findings), [`${CONF}|base:frame-ancestors`]);
  assert.match(findings[0].problem, /^src-tauri\/tauri\.conf\.json:4 base CSP has no frame-ancestors 'none'/);
  assert.deepEqual(run("shell-csp", t, { [CONF]: conf([], { csp: GOOD_CSP }) }), [], "style-src 'unsafe-inline' and ipc: are fine");
  assert.deepEqual(run("shell-csp", t, { [CONF]: conf([], { csp: { "default-src": "'self'", "frame-ancestors": ["'none'"], "connect-src": "'self'" } }) }), [], "a directive object is read too");
});

test("R4: every directive of the baseline has its own finding", (t) => {
  const subjectsOf = (csp) => subjects(run("shell-csp", t, { [CONF]: conf([], { csp }) }));
  assert.deepEqual(subjectsOf("script-src 'self'; frame-ancestors 'none'; connect-src 'self'"), ["base:default-src"]);
  assert.deepEqual(subjectsOf("default-src 'self'; script-src 'self' 'unsafe-inline'; frame-ancestors 'none'; connect-src 'self'"), ["base:script-src-unsafe-inline"]);
  assert.deepEqual(subjectsOf("default-src 'self'; script-src 'self' 'unsafe-eval'; frame-ancestors 'none'; connect-src 'self'"), ["base:script-src-unsafe-eval"]);
  assert.deepEqual(subjectsOf("default-src 'self' 'unsafe-inline'; frame-ancestors 'none'; connect-src 'self'"), ["base:script-src-unsafe-inline"], "with no script-src, default-src stands in");
  assert.deepEqual(subjectsOf("default-src 'self' 'unsafe-inline'; script-src 'self'; frame-ancestors 'none'; connect-src 'self'"), [], "script-src wins over default-src");
  assert.deepEqual(subjectsOf("default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; frame-ancestors 'none'; connect-src 'self'"), [], "wasm-unsafe-eval is not unsafe-eval");
  assert.deepEqual(subjectsOf("default-src 'self'; script-src 'self'; frame-ancestors 'none'"), ["base:connect-src"]);
  assert.deepEqual(subjectsOf("default-src 'self'; frame-ancestors 'self'; connect-src 'self'"), ["base:frame-ancestors"]);
  assert.deepEqual(run("shell-csp", t, { [CONF]: conf([]) }).map((f) => f.subject), ["base:csp-missing"]);
  assert.deepEqual(subjectsOf(""), ["base:csp-missing"]);
});

test("R4: a platform file is judged when its own patch restates csp, and not otherwise", (t) => {
  const base = { [CONF]: conf([], { csp: GOOD_CSP }) };
  assert.deepEqual(run("shell-csp", t, { ...base, [`${SRC}/tauri.macos.conf.json`]: { bundle: {} } }), []);
  const findings = run("shell-csp", t, { ...base, [`${SRC}/tauri.ios.conf.json`]: { app: { security: { csp: "default-src 'self'; connect-src 'self'" } } } });
  assert.deepEqual(ids(findings), [`${SRC}/tauri.ios.conf.json|ios:frame-ancestors`]);
  const noFrame = "default-src 'self'; connect-src 'self'";
  const inherits = run("shell-csp", t, { [CONF]: conf([], { csp: noFrame }), [`${SRC}/tauri.macos.conf.json`]: { bundle: {} }, [`${SRC}/tauri.windows.conf.json`]: { app: { windows: [] } } });
  assert.deepEqual(ids(inherits), [`${CONF}|base:frame-ancestors`], "platform files that do not restate csp inherit the base finding and do not repeat it");
  const objectPatch = run("shell-csp", t, { [CONF]: conf([], { csp: { "default-src": "'self'", "frame-ancestors": "'none'", "connect-src": "'self'" } }), [`${SRC}/tauri.linux.conf.json`]: { app: { security: { csp: { "script-src": "'self' 'unsafe-inline'" } } } } });
  assert.deepEqual(ids(objectPatch), [`${SRC}/tauri.linux.conf.json|linux:script-src-unsafe-inline`], "object CSPs merge by key, so the patch adds script-src to the base's directives");
});

// ---- R5 shell-grants ---------------------------------------------------------------------------------------------------------
test("R5 shell-grants: core:window:allow-* is refused while rk_shell_window is registered", (t) => {
  const caps = { permissions: ["core:default", "core:window:default", "core:window:allow-minimize", { identifier: "core:window:allow-close" }, "core:webview:allow-print", "allow-rk-shell-window"] };
  const files = (rust, extra = {}) => ({ [CONF]: conf([]), [`${SRC}/capabilities/default.json`]: caps, [`${SRC}/src/lib.rs`]: rust, ...extra });
  const findings = wellFormed(run("shell-grants", t, files(RS_HANDLER)));
  assert.deepEqual(ids(findings), [`${SRC}/capabilities/default.json|core:window:allow-close`, `${SRC}/capabilities/default.json|core:window:allow-minimize`]);
  assert.match(findings.find((f) => f.subject.endsWith("minimize")).problem, /^src-tauri\/capabilities\/default\.json:1 grants core:window:allow-minimize/);
  assert.equal(run("shell-grants", t, files(RS_LIST("rk_shell_window"))).length, 2, "a hand-listed rk_shell_window counts");
  assert.deepEqual(run("shell-grants", t, files(RS_LIST("rk_shell_quit"))), [], "the shell's window command is not registered");
  assert.deepEqual(run("shell-grants", t, files("pub fn run() {}\n")), []);
});

test("R5: inline capabilities of the conf and several capability documents are read", (t) => {
  const files = {
    [CONF]: conf([], { capabilities: ["default", { identifier: "inline", windows: ["main"], permissions: ["core:window:allow-center", "core:event:default"] }] }),
    [`${SRC}/capabilities/a.json`]: [{ identifier: "a", permissions: ["core:window:allow-show"] }],
    [`${SRC}/capabilities/b.json`]: "{ broken",
    [`${SRC}/capabilities/c.json`]: "null",
    [`${SRC}/src/lib.rs`]: RS_HANDLER,
  };
  assert.deepEqual(ids(run("shell-grants", t, files)), [`${CONF}|core:window:allow-center`, `${SRC}/capabilities/a.json|core:window:allow-show`].sort());
});

// ---- R6 qa-window-label ------------------------------------------------------------------------------------------------------
const SHELL_HUB = 'fn shell_plugin() -> TauriPlugin<Wry> {\n    let shell = Shell::builder("heardright").main_label("hub").build();\n    ShellPlugin::builder().shell(shell).lifecycle_only()\n}\n';
const CONTROL_BARE = 'fn qa(mut builder: Builder<Wry>) {\n    if let Some(control) =\n        qa_commands::register(rightkit_qa::embedded::Control::<tauri::Wry>::new()).build_if_enabled()\n    {\n        builder = builder.plugin(control);\n    }\n}\n';
const HUB_CONF = conf([win({ label: "hub", visible: false })]);

test("R6 qa-window-label: main_label(\"hub\") with a bare Control is exactly one finding that names both files", (t) => {
  const findings = wellFormed(run("qa-window-label", t, { [CONF]: HUB_CONF, [`${SRC}/src/app_run.rs`]: SHELL_HUB, [`${SRC}/src/qa.rs`]: CONTROL_BARE }));
  assert.equal(findings.length, 1);
  assert.deepEqual(ids(findings), [`${SRC}/src/qa.rs|control-label`]);
  assert.match(findings[0].problem, /^src-tauri\/src\/qa\.rs:3 registers the QA Control with no \.window\(\.\.\), so it binds "main", but src-tauri\/src\/app_run\.rs:2 sets the shell's main window to "hub"/);
  assert.equal(findings[0].fix, 'add .window("hub") to the Control registration at src-tauri/src/qa.rs:3');
});

test("R6: .window(\"hub\") on the Control, in the chain or on its binding, passes; both defaulted passes", (t) => {
  const shell = { [CONF]: HUB_CONF, [`${SRC}/src/app_run.rs`]: SHELL_HUB };
  const USE = "use rightkit_qa::embedded::Control;\n";
  const inChain = USE + 'fn qa(mut b: Builder<Wry>) {\n    if let Some(c) = qa_commands::register(Control::<tauri::Wry>::new().window("hub")).build_if_enabled() { b = b.plugin(c); }\n}\n';
  const afterRegister = USE + 'fn qa(mut b: Builder<Wry>) {\n    if let Some(c) = qa_commands::register(Control::<tauri::Wry>::new()).window("hub").build_if_enabled() { b = b.plugin(c); }\n}\n';
  const bound = USE + 'fn qa(mut b: Builder<Wry>) {\n    let control = Control::<tauri::Wry>::new();\n    let control = control.command("x", |_a, _b| Ok(()));\n    let control = control.window("hub");\n    if let Some(c) = control.build_if_enabled() { b = b.plugin(c); }\n}\n';
  for (const qa of [inChain, afterRegister, bound]) assert.deepEqual(run("qa-window-label", t, { ...shell, [`${SRC}/src/qa.rs`]: qa }), [], qa);
  const defaults = { [CONF]: conf([win({ visible: false })]), [`${SRC}/src/app_run.rs`]: SHELL_HUB.replace('.main_label("hub")', ""), [`${SRC}/src/qa.rs`]: USE + 'fn qa() { let c = Control::default(); let d = Control::new(); }\n' };
  assert.deepEqual(run("qa-window-label", t, defaults), [], "no main_label and a bare Control are both main");
  const other = run("qa-window-label", t, { ...defaults, [`${SRC}/src/qa.rs`]: USE + 'fn qa() { let c = Control::new().window("other"); }\n' });
  assert.deepEqual(run("qa-window-label", t, { ...defaults, [`${SRC}/src/qa.rs`]: 'use some_crate::Control;\nfn qa() { let c = Control::new().window("other"); }\n' }), [], "a Control that is not rightkit-qa's is not the QA control");
  assert.deepEqual(ids(other), [`${SRC}/src/qa.rs|control-label`], "an explicit label that differs from the shell's main label");
  assert.match(other[0].fix, /\.window\("main"\)/);
});

test("R6: the main label is read through a string const, and an unresolvable one is not guessed", (t) => {
  const shell = 'const HUB_LABEL: &str = "main";\nfn shell_plugin() { Shell::builder("pulse-hub").main_label(HUB_LABEL).build(); }\n';
  assert.deepEqual(run("qa-window-label", t, { [CONF]: conf([win({ create: false })]), [`${SRC}/src/lib.rs`]: `${shell}fn open(app: &AppHandle<Wry>) { rightkit_tauri_shell::windows::builder(app, HUB_LABEL, url); }\n${CONTROL_BARE}` }), []);
  assert.deepEqual(subjects(run("qa-window-label", t, { [CONF]: HUB_CONF, [`${SRC}/src/lib.rs`]: shell.replace('"main"', '"hub"') + CONTROL_BARE })), ["control-label"]);
  assert.deepEqual(run("qa-window-label", t, { [CONF]: HUB_CONF, [`${SRC}/src/lib.rs`]: 'fn s() { Shell::builder("x").main_label(compute()).build(); }\n' + CONTROL_BARE }), [], "main_label(compute()) is not a literal");
});

test("R6: a main label that no window carries is main-label-not-created; a code-built window or any config target counts", (t) => {
  const shellOnly = { [`${SRC}/src/app_run.rs`]: SHELL_HUB };
  const findings = wellFormed(run("qa-window-label", t, { [CONF]: conf([win({ visible: false })]), ...shellOnly }));
  assert.deepEqual(ids(findings), [`${SRC}/src/app_run.rs|main-label-not-created`]);
  assert.match(findings[0].problem, /^src-tauri\/src\/app_run\.rs:2 makes "hub" the shell's main window, but no window with that label is created \(windows labelled "main"\)/);
  assert.deepEqual(run("qa-window-label", t, { [CONF]: conf([win({ visible: false })]), [`${SRC}/src/app_run.rs`]: `${SHELL_HUB}fn open(app: &AppHandle<Wry>) { rightkit_tauri_shell::windows::builder(app, "hub", WebviewUrl::App("/".into())).build(); }\n` }), [], "windows::builder creates it");
  assert.deepEqual(run("qa-window-label", t, { [CONF]: conf([win({ visible: false })]), [`${SRC}/src/app_run.rs`]: `${SHELL_HUB}fn open(app: &App) { WebviewWindowBuilder::new(app, "hub", url).build(); }\n` }), []);
  assert.deepEqual(run("qa-window-label", t, { [CONF]: conf([]), [`${SRC}/tauri.windows.conf.json`]: { app: { windows: [win({ label: "hub", visible: false })] } }, ...shellOnly }), [], "a platform conf's window counts");
  assert.deepEqual(run("qa-window-label", t, { [CONF]: conf([]), ...shellOnly }), [], "an app that declares and builds no window at all says nothing here");
  assert.deepEqual(run("qa-window-label", t, { [CONF]: conf([win({ visible: false })]), [`${SRC}/src/app_run.rs`]: `${SHELL_HUB}fn open(app: &AppHandle<Wry>, label: &str) { rightkit_tauri_shell::windows::builder(app, label, url); }\n` }), [], "a label known only at run time is not guessed");
  assert.deepEqual(run("qa-window-label", t, { [CONF]: HUB_CONF, [`${SRC}/src/qa.rs`]: CONTROL_BARE }), [], "an app that never builds the shell has no main label to compare");
});

// ---- R7 devtools-declaration -------------------------------------------------------------------------------------------------
const CARGO_ON = '[package]\nname = "app"\n\n[dependencies]\nrightkit-tauri-shell = { version = "=0.5.1", default-features = false, features = ["wv2-0_39", "content-devtools"] }\n';
const CARGO_OFF = '[package]\nname = "app"\n\n[dependencies]\nrightkit-tauri-shell = { version = "=0.5.1", features = ["wv2-0_39"] }\n';
const BUILD_ACL = 'fn main() {\n    let attributes = tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));\n    tauri_build::try_build(attributes).unwrap();\n}\n';
const BUILD_PLAIN = "fn main() { tauri_build::build() }\n";
const RS_POLICY = 'fn plugin() { Shell::builder("v").devtools_policy(DevToolsPolicy::ContentOnly).build(); }\n';
const RS_NO_POLICY = 'fn plugin() { Shell::builder("v").build(); }\n';
const RELEASE_ON = 'export default {\n  schema: 1,\n  app: "viewright",\n  devtools: "content-only",\n};\n';
const RELEASE_OFF = 'export default {\n  schema: 1,\n  app: "viewright",\n  // devtools: "content-only",\n};\n';
const devtoolsApp = ({ release = RELEASE_ON, cargo = CARGO_ON, build = BUILD_ACL, rust = RS_POLICY } = {}) => ({
  "right-release.config.mjs": release, [CONF]: conf([]), [`${SRC}/Cargo.toml`]: cargo, [`${SRC}/build.rs`]: build, [`${SRC}/src/lib.rs`]: rust,
});

test("R7 devtools-declaration: five sites that agree pass", (t) => {
  assert.deepEqual(run("devtools-declaration", t, devtoolsApp(), { contentOnlyApps: ["viewright"] }), []);
  assert.deepEqual(run("devtools-declaration", t, devtoolsApp(), {}), [], "this checkout's own allow-list names viewright");
  const json = { ...devtoolsApp(), "right-release.config.mjs": undefined, "right-release.config.json": '{ "app": "viewright", "devtools": "content-only" }' };
  delete json["right-release.config.mjs"];
  assert.deepEqual(run("devtools-declaration", t, json, { contentOnlyApps: ["viewright"] }), [], "a JSON release config declares it too");
});

test("R7: an app with none of the declaration, the cargo feature or the runtime policy has nothing to cross-check", (t) => {
  assert.deepEqual(run("devtools-declaration", t, devtoolsApp({ release: 'export default { app: "x", devtools: "denied" };\n', cargo: CARGO_OFF, build: BUILD_PLAIN, rust: RS_NO_POLICY })), []);
  assert.deepEqual(run("devtools-declaration", t, { [CONF]: conf([]) }), []);
});

test("R7: ViewRight's shape, content-devtools on and no devtools key in the release config, is a release-config finding", (t) => {
  const findings = wellFormed(run("devtools-declaration", t, devtoolsApp({ release: RELEASE_OFF }), { contentOnlyApps: ["viewright"] }));
  assert.deepEqual(ids(findings), ["right-release.config.mjs|release-config"]);
  assert.match(findings[0].problem, /^right-release\.config\.mjs:1 the release config does not declare devtools: "content-only"/);
  assert.match(findings[0].problem, /Cargo\.toml selects content-devtools, the Rust calls \.devtools_policy\(ContentOnly\)/);
  assert.equal(findings[0].fix, 'add devtools: "content-only" to right-release.config.mjs');
});

test("R7: each missing site is its own finding, filed where it belongs", (t) => {
  const one = (options, extra) => ids(run("devtools-declaration", t, devtoolsApp(options), { contentOnlyApps: ["viewright"], ...extra }));
  assert.deepEqual(one({ release: RELEASE_OFF }), ["right-release.config.mjs|release-config"]);
  assert.deepEqual(one({}, { contentOnlyApps: ["heardright"] }), ["packages/release/devtools-policy.json|allow-list"]);
  assert.deepEqual(one({ cargo: CARGO_OFF }), [`${SRC}/Cargo.toml|cargo-feature`]);
  assert.deepEqual(one({ build: BUILD_PLAIN }), [`${SRC}/build.rs|acl-manifest`]);
  assert.deepEqual(one({ rust: RS_NO_POLICY }), [`${SRC}/src/lib.rs|runtime-policy`]);
  const allMissing = ids(run("devtools-declaration", t, devtoolsApp({ cargo: CARGO_OFF, build: BUILD_PLAIN, rust: RS_NO_POLICY }), { contentOnlyApps: [] }));
  assert.deepEqual(allMissing, [`${SRC}/Cargo.toml|cargo-feature`, `${SRC}/build.rs|acl-manifest`, `${SRC}/src/lib.rs|runtime-policy`, "packages/release/devtools-policy.json|allow-list"].sort(), "declared but nothing else is there");
  const noBuild = devtoolsApp();
  delete noBuild[`${SRC}/build.rs`];
  assert.deepEqual(one({}, {}).length, 0);
  assert.deepEqual(ids(run("devtools-declaration", t, noBuild, { contentOnlyApps: ["viewright"] })), [`${SRC}/build.rs|acl-manifest`], "no build.rs declares no manifest");
});

test("R7: the cargo feature alone triggers the check, an app feature that forwards it counts, and a const policy is followed", (t) => {
  const onlyCargo = { [CONF]: conf([]), "right-release.config.mjs": 'export default { app: "other" };\n', [`${SRC}/Cargo.toml`]: CARGO_ON, [`${SRC}/build.rs`]: BUILD_ACL, [`${SRC}/src/lib.rs`]: RS_POLICY };
  assert.deepEqual(subjects(run("devtools-declaration", t, onlyCargo, { contentOnlyApps: ["viewright"] })), ["allow-list", "release-config"]);
  const viaFeature = '[package]\nname = "a"\n[features]\ncontent-devtools = ["rightkit-tauri-shell/content-devtools"]\n[dependencies]\nrightkit-tauri-shell = "=0.5.1"\n';
  assert.deepEqual(run("devtools-declaration", t, devtoolsApp({ cargo: viaFeature }), { contentOnlyApps: ["viewright"] }), []);
  const constPolicy = 'const POLICY: DevToolsPolicy = DevToolsPolicy::ContentOnly;\nfn plugin() { Shell::builder("v").devtools_policy(POLICY).build(); }\n';
  assert.deepEqual(run("devtools-declaration", t, devtoolsApp({ rust: constPolicy }), { contentOnlyApps: ["viewright"] }), []);
  const denied = 'const POLICY: DevToolsPolicy = DevToolsPolicy::Denied;\nfn plugin() { Shell::builder("v").devtools_policy(POLICY).build(); }\n';
  assert.deepEqual(subjects(run("devtools-declaration", t, devtoolsApp({ rust: denied }), { contentOnlyApps: ["viewright"] })), ["runtime-policy"]);
  const noApp = devtoolsApp({ release: 'export default { devtools: "content-only" };\n' });
  assert.deepEqual(ids(run("devtools-declaration", t, noApp, { contentOnlyApps: ["viewright"] })), ["right-release.config.mjs|allow-list"], "a release config that names no app cannot be on the allow-list");
});

test("contentDevtoolsCrate answers like devtools-policy.mjs selectedContentDevtoolsCrate on every manifest shape", () => {
  const manifests = [
    CARGO_ON, CARGO_OFF,
    '[dependencies]\nrightkit-shell = { version = "1", features = ["content-devtools"] }\n',
    '[dependencies.rightkit-tauri-shell]\nversion = "1"\nfeatures = ["a", "content-devtools"]\n',
    '[target.\'cfg(windows)\'.dependencies]\nrightkit-tauri-shell = { version = "1", features = [\n  "x",\n  "content-devtools",\n] }\n',
    '[features]\ndevtools = ["rightkit-tauri-shell/content-devtools"]\n[dependencies]\nrightkit-tauri-shell = "1"\n',
    '[features]\ndevtools = ["rightkit-tauri-shell?/content-devtools"]\n[dependencies]\nrightkit-tauri-shell = { version = "1", optional = true }\n',
    '[features]\ndevtools = ["other/content-devtools"]\n[dependencies]\nrightkit-tauri-shell = "1"\n',
    '[dependencies]\n# rightkit-tauri-shell = { features = ["content-devtools"] }\nrightkit-tauri-shell = "1" # content-devtools\n',
    '[dependencies]\nrightkit-qa = { version = "1", features = ["content-devtools"] }\n',
    "",
  ];
  for (const manifest of manifests) assert.equal(contentDevtoolsCrate(manifest), selectedContentDevtoolsCrate(manifest), manifest);
  assert.equal(contentDevtoolsCrate(CARGO_ON), "rightkit-tauri-shell");
  assert.equal(contentDevtoolsCrate(undefined), null);
});

// ---- R8 shell-lifecycle-external ---------------------------------------------------------------------------------------------
const EXTERNAL = "fn sign_in(app: &AppHandle<Wry>, url: Url) { windows::builder(app, SIGN_IN_LABEL, WebviewUrl::External(url)).build(); }\n";

test("R8 shell-lifecycle-external: lifecycle_only next to a WebviewUrl::External window fails, host_windows passes", (t) => {
  const lifecycle = "fn plugin() -> TauriPlugin<Wry> {\n    ShellPlugin::builder().shell(shell).lifecycle_only()\n}\n";
  const findings = wellFormed(run("shell-lifecycle-external", t, { [CONF]: conf([]), [`${SRC}/src/main.rs`]: lifecycle, [`${SRC}/src/sign_in.rs`]: EXTERNAL }));
  assert.deepEqual(ids(findings), [`${SRC}/src/main.rs|lifecycle-only+external`], "the finding sits on the lifecycle call");
  assert.match(findings[0].problem, /^src-tauri\/src\/main\.rs:2 installs the shell with lifecycle_only\(\) while the app opens an external-URL window \(src-tauri\/src\/sign_in\.rs:1\)/);
  assert.match(findings[0].fix, /ShellPlugin::builder\(\)\.shell\(\.\.\)\.host_windows\(\)/);
  const hostWindows = lifecycle.replace("lifecycle_only", "host_windows");
  assert.deepEqual(run("shell-lifecycle-external", t, { [CONF]: conf([]), [`${SRC}/src/main.rs`]: hostWindows, [`${SRC}/src/sign_in.rs`]: EXTERNAL }), []);
  assert.deepEqual(run("shell-lifecycle-external", t, { [CONF]: conf([]), [`${SRC}/src/main.rs`]: lifecycle }), [], "no external window: lifecycle_only is a profile choice, not a hole");
});

test("R8: lifecycle_plugin and ModuleSet::lifecycle_only count; comments and definitions do not", (t) => {
  const external = { [`${SRC}/src/sign_in.rs`]: EXTERNAL };
  assert.deepEqual(subjects(run("shell-lifecycle-external", t, { [CONF]: conf([]), ...external, [`${SRC}/src/lib.rs`]: "fn a() { builder.plugin(shell.build().lifecycle_plugin()); }\n" })), ["lifecycle-only+external"]);
  assert.deepEqual(subjects(run("shell-lifecycle-external", t, { [CONF]: conf([]), ...external, [`${SRC}/src/lib.rs`]: "fn a() { let m = ModuleSet::lifecycle_only(); }\n" })), ["lifecycle-only+external"]);
  assert.deepEqual(run("shell-lifecycle-external", t, { [CONF]: conf([]), ...external, [`${SRC}/src/lib.rs`]: "// ShellPlugin::builder().lifecycle_only()\npub fn lifecycle_only() {}\n" }), []);
  assert.deepEqual(run("shell-lifecycle-external", t, { [CONF]: conf([]), [`${SRC}/src/sign_in.rs`]: `// ${EXTERNAL}`, [`${SRC}/src/lib.rs`]: "fn a() { s.lifecycle_only(); }\n" }), [], "a commented-out external window is not one");
});

// ---- R9 legal-receipt-native -------------------------------------------------------------------------------------------------
const ONB = (attrs, extraImports = "") => `import { Onboarding${extraImports} } from "@rightkit/onboarding";\nexport function Setup() {\n  return (\n    <Onboarding\n      settings={settings}\n${attrs}      onComplete={() => done()}\n    />\n  );\n}\n`;
const app9 = (files) => ({ [CONF]: conf([]), ...files });

test("R9 legal-receipt-native: a native receipt port passes, directly, through a binding, in another file and inside an object", (t) => {
  const direct = ONB("      legalReceipt={createNativeLegalReceiptPort({ invoke })}\n", ", createNativeLegalReceiptPort");
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Setup.tsx": direct })), []);
  const bound = "export const legalReceiptPort = createNativeLegalReceiptPort({ invoke }) as Port;\n";
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/ipc.ts": bound, "src/Setup.tsx": ONB("      legalReceipt={legalReceiptPort}\n") })), [], "a binding in another file that is built from the native port");
  const inObject = ONB("      legalReceipt={{ read: legalReceiptPort.read, create: async (request) => legalReceiptPort.create(request), reset: legalReceiptPort.reset }}\n");
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/ipc.ts": bound, "src/Setup.tsx": inObject })), [], "MailRight's object over the native port");
  const memo = "const port = useMemo(() => createNativeLegalReceiptPort({ invoke }), []);\n" + ONB("      legalReceipt={port}\n");
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Setup.tsx": memo })), []);
});

test("R9: a hand-written port fails and names its file:line; a chained builder names the function that writes the record", (t) => {
  const handWritten = 'import type { LegalReceiptPort } from "@rightkit/onboarding";\n\nexport const legalReceiptPort: LegalReceiptPort = {\n  read: async () => load(),\n  write: async (record) => save(record),\n};\n';
  const findings = wellFormed(run("legal-receipt-native", t, app9({ "src/lib/onboarding.ts": handWritten, "src/Setup.tsx": ONB("      legalReceipt={legalReceiptPort}\n") })));
  assert.deepEqual(ids(findings), ["src/Setup.tsx|hand-written-port"]);
  assert.match(findings[0].problem, /^src\/Setup\.tsx:4 <Onboarding> takes legalReceipt=\{legalReceiptPort\}, a hand-written port \(src\/lib\/onboarding\.ts:3\)/);
  assert.match(findings[0].fix, /createNativeLegalReceiptPort\(\{ invoke \}\)/);
  const chained = {
    "src/lib/backend.ts": "export function createLegalReceipt(onAccepted: () => void): LegalReceiptPort {\n  return { read: () => readStore(), write: async (r) => { await writeStore(r); onAccepted(); } };\n}\n",
    "src/Setup.tsx": 'const legalReceipt = useMemo(() => createLegalReceipt(() => setDone(true)), []);\n' + ONB("      legalReceipt={legalReceipt}\n"),
  };
  const chain = run("legal-receipt-native", t, app9(chained));
  assert.deepEqual(subjects(chain), ["hand-written-port"]);
  assert.match(chain[0].problem, /\(src\/lib\/backend\.ts:1\)/, "the port is the function that writes, not the memo that calls it");
  const inline = run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB("      legalReceipt={{ read: () => null, write: async () => {} }}\n") }));
  assert.deepEqual(subjects(inline), ["hand-written-port"]);
});

test("R9: no legalReceipt is settings-copy; legalMode none or notice, a spread and an unknown name are not judged", (t) => {
  const findings = wellFormed(run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB("") })));
  assert.deepEqual(ids(findings), ["src/Setup.tsx|settings-copy"]);
  assert.match(findings[0].problem, /^src\/Setup\.tsx:4 <Onboarding> has no legalReceipt/);
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB('      legalMode="none"\n') })), []);
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB("      legalMode={'notice'}\n") })), []);
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB("      legalMode={mode}\n") })), [], "a computed mode cannot be judged from the file");
  assert.deepEqual(subjects(run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB('      legalMode="gate"\n') }))), ["settings-copy"]);
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB("      {...props}\n") })), [], "props spread in: the receipt may be among them");
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Setup.tsx": ONB("      legalReceipt={receiptFromProps}\n") })), [], "a name this app does not define");
});

test("R9: only the Onboarding of @rightkit/onboarding in shipped renderer code counts", (t) => {
  const other = 'import { Onboarding } from "./mine";\nexport const a = <Onboarding />;\n';
  const typeOnly = 'import type { Onboarding } from "@rightkit/onboarding";\nexport const b = <Onboarding />;\n';
  const files = { "src/a.tsx": other, "src/b.tsx": typeOnly, "src/Setup.test.tsx": ONB(""), "scripts/qa.tsx": ONB(""), "src/c.ts": ONB("") };
  assert.deepEqual(run("legal-receipt-native", t, app9(files)), []);
  const aliased = 'import { Onboarding as Wizard } from "@rightkit/onboarding";\nexport const a = <Wizard settings={s} />;\n';
  assert.deepEqual(subjects(run("legal-receipt-native", t, app9({ "src/Setup.tsx": aliased }))), ["settings-copy"], "an aliased import is still the Onboarding");
});

test("R9: a LegalGate that stamps the acceptance with the webview clock needs a native receipt somewhere in the app", (t) => {
  const gate = (onAgree) => `import { LegalGate } from "@rightkit/legal-ui";\nasync function handleAgree({ eligibilityBasis }) {\n  await invoke("apply_settings_patch", { patch: { legal_accepted_at: new Date().toISOString(), basis: eligibilityBasis } });\n}\nexport const G = () => (\n  <LegalGate appName="App" onAgree={${onAgree}} />\n);\n`;
  const findings = wellFormed(run("legal-receipt-native", t, app9({ "src/Gate.tsx": gate("handleAgree") })));
  assert.deepEqual(ids(findings), ["src/Gate.tsx|legal-gate-without-native-receipt"]);
  assert.match(findings[0].problem, /^src\/Gate\.tsx:6 <LegalGate> records the acceptance in the app's own store with a webview-clock timestamp/);
  assert.deepEqual(subjects(run("legal-receipt-native", t, app9({ "src/Gate.tsx": gate("async () => { await save(Date.now()); }") }))), ["legal-gate-without-native-receipt"], "an inline handler too");
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Gate.tsx": gate("handleAgree"), "src/state/legal.ts": "export const port = createNativeLegalReceiptPort({ invoke });\n" })), [], "the app has a native receipt port");
  assert.deepEqual(run("legal-receipt-native", t, app9({ "src/Gate.tsx": gate("() => setReviewing(false)").replace(/async function handleAgree[\s\S]*?\n}\n/, "") })), [], "a review gate whose agree writes nothing is no acceptance store");
});

// ---- no rule throws ----------------------------------------------------------------------------------------------------------
test("a rule that fails on one app reports a rule-error finding with an identity for it, and still judges the other apps", (t) => {
  // an allow-list that is not a list makes the devtools rule throw on an app that declares content-only DevTools
  const files = { ...devtoolsApp(), "other/right-release.config.json": '{ "app": "other" }', "other/tauri.conf.json": conf([win({ visible: false })]), "other/src/lib.rs": RS_NO_POLICY };
  const findings = wellFormed(run("devtools-declaration", t, files, { contentOnlyApps: {} }));
  assert.deepEqual(ids(findings), [`${CONF}|rule-error`], "the app that failed is a finding; the app with nothing to cross-check says nothing");
  assert.match(findings[0].problem, /^src-tauri\/tauri\.conf\.json:1 the shell-contract rule failed on this app \(/);
});

test("no rule throws on odd content: bad JSON, odd Rust and JSX, binary files, empty files, wrong types", (t) => {
  const junk = {
    [CONF]: "{ not json", [`${SRC}/tauri.macos.conf.json`]: "null", [`${SRC}/tauri.windows.conf.json`]: '{"app": 3}', [`${SRC}/tauri.linux.conf.json`]: '{"app": {"windows": "x", "security": {"csp": 7, "capabilities": 4}}}',
    [`${SRC}/capabilities/a.json`]: '{"permissions": [null, 4, {"identifier": 5}, "core:window:allow-x"]}',
    [`${SRC}/Cargo.toml`]: "[dependencies\nrightkit-tauri-shell = {", [`${SRC}/build.rs`]: "", [`${SRC}/src/lib.rs`]: 'fn ( { .plugin( generate_handler![ "unterminated\n Control::<(',
    [`${SRC}/src/bin.rs`]: "\u0000\u0001\uffff", [`${SRC}/src/empty.rs`]: "", "right-release.config.mjs": "export default {{{", "src/a.tsx": '<Onboarding legalReceipt={ import { Onboarding } from "@rightkit/onboarding"; <Onboarding legalReceipt={x',
    "src/b.ts": "rk_shell_ `unterminated", "src/c.tsx": 'import { LegalGate } from "@rightkit/legal-ui"; <LegalGate onAgree=',
  };
  const { root, files } = tree(t, junk);
  for (const rule of SHELL_CONTRACT_RULES) wellFormed(rule.run({ root, files, shellIndex: "export function (" }));
  wellFormed(SHELL_CONTRACT_RULES.flatMap((rule) => rule.run({ root, files: [...files, "missing/tauri.conf.json", "gone/src/x.rs"] })));
  const emptyApp = tree(t, { "tauri.conf.json": "", "src/lib.rs": "", "src/x.tsx": "" });
  for (const rule of SHELL_CONTRACT_RULES) wellFormed(rule.run(emptyApp));
});
