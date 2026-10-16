import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

// GUI apps & background services built on RightKit must never flash a console window on Windows.
// A console-subsystem child of a windowless parent gets a fresh visible console unless its spawner
// passes CREATE_NO_WINDOW, so every crate-source `std::process::Command::new` must, within the
// lines that build it, either hide the window or be compiled only off Windows. `OwnedCommand`
// (rightkit-child-process) hides by default. An explicit `// windowless: <reason>` comment just above a
// spawn documents a site that hides it elsewhere (e.g. hands the Command to OwnedCommand).
const crates = path.resolve(import.meta.dirname, "../../crates");
const SPAWN = /(?<![A-Za-z_])Command::new\(/;
const HIDDEN = /creation_flags|CREATE_NO_WINDOW|\bhidden\(|hide_background_process|spawn_hidden_logged|OwnedCommand::from_command|windows_hide/;
const LAUNCH = /\.(?:spawn|output|status)\(|from_command\(|spawn_hidden_logged\(|hide_background_process\(|\bfinish\(/;
// OwnedCommand's own constructor: `from_command` starts every command hidden on Windows (windows_hidden: true).
const OWNED_COMMAND_CONSTRUCTOR = { file: "rightkit-child-process/src/owned.rs", line: /^\s*(?:let\s+\w+\s*=\s*)?Self::from_command\(Command::new\(program\)\);?\s*$/ };
const MACOS_ONLY_FILE = /(^|[\\/])(macos([\\/]|\.rs$)|mac\.rs$|apple_helper\.rs$)/;

function rustFiles(directory) {
  const out = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const file = path.join(directory, entry.name);
    if (entry.isDirectory()) out.push(...rustFiles(file));
    else if (entry.name.endsWith(".rs")) out.push(file);
  }
  return out;
}

// ---- cfg evaluation: is code under this attribute compiled into a normal (non-test) Windows build? ----
// Three-valued: true / false / null (unknown). Unknown (features, debug_assertions, custom cfgs) counts as
// reachable, so only code that is certainly absent from a Windows build is skipped.
function tokenize(expression) {
  return expression.match(/"(?:[^"\\]|\\.)*"|[A-Za-z_][A-Za-z0-9_]*|[(),=]/g) ?? [];
}
export function cfgOnWindows(expression) {
  const tokens = tokenize(expression);
  let at = 0;
  const parse = () => {
    const name = tokens[at++];
    if (tokens[at] === "(") {
      at += 1;
      const args = [];
      while (tokens[at] !== ")" && at < tokens.length) { args.push(parse()); if (tokens[at] === ",") at += 1; }
      at += 1;
      if (name === "not") return args[0] === null ? null : !args[0];
      if (name === "all") return args.includes(false) ? false : args.includes(null) ? null : true;
      if (name === "any") return args.includes(true) ? true : args.includes(null) ? null : false;
      return null;
    }
    if (tokens[at] === "=") {
      at += 1;
      const value = (tokens[at++] ?? "").replaceAll('"', "");
      if (name === "target_os") return value === "windows";
      if (name === "target_family") return value === "windows";
      if (name === "target_vendor") return value === "pc" ? null : false;
      return null;
    }
    if (name === "windows") return true;
    if (name === "unix" || name === "test") return false;
    return null;
  };
  return parse();
}

const code = (line) => line.replace(/"(?:[^"\\]|\\.)*"/g, '""').replace(/'(?:[^'\\]|\\.)'/g, "' '").replace(/\/\/.*$/, "");

/** `#[cfg(<expr>)]` (or `#![cfg(<expr>)]`) on this line: { expression, rest } with rest the text after the attribute. */
function cfgAttribute(line) {
  const start = line.search(/#!?\[cfg\(/);
  if (start < 0) return null;
  const open = line.indexOf("(", start);
  let depth = 0;
  for (let i = open; i < line.length; i += 1) {
    if (line[i] === "(") depth += 1;
    else if (line[i] === ")" && --depth === 0) {
      const after = line.slice(i + 1).replace(/^\s*\]/, "");
      return { inner: line.slice(start).startsWith("#!"), expression: line.slice(open + 1, i), rest: after.trim() };
    }
  }
  return null;
}

/** Which lines of a Rust file are certainly not compiled into a normal Windows build. */
export function linesAbsentOnWindows(lines) {
  const absent = new Array(lines.length).fill(false);
  for (let index = 0; index < lines.length; index += 1) {
    const attr = cfgAttribute(lines[index]);
    if (!attr || attr.inner || cfgOnWindows(attr.expression) !== false) continue;
    // The gated item starts after the attribute (same line) or on the next non-attribute, non-blank line.
    let start = index;
    let text = attr.rest;
    while (!text || /^#\[/.test(text.trim())) {
      start += 1;
      if (start >= lines.length) break;
      text = lines[start].trim();
      if (text.startsWith("//")) text = "";
    }
    if (start >= lines.length) continue;
    let depth = 0;
    let opened = false;
    let end = start;
    for (; end < lines.length; end += 1) {
      const body = end === start && start === index ? attr.rest : code(lines[end]);
      for (const ch of code(body)) {
        if (ch === "{") { depth += 1; opened = true; } else if (ch === "}") depth -= 1;
      }
      if (opened ? depth <= 0 : code(body).includes(";")) break;
    }
    for (let l = index; l <= Math.min(end, lines.length - 1); l += 1) absent[l] = true;
    index = Math.min(end, lines.length - 1);
  }
  return absent;
}

/** Files and directories that are wholly gated away from Windows: `#![cfg(..)]` inside, or `#[cfg(..)] mod x;` outside. */
function gatedPaths(files) {
  const gated = new Set();
  for (const file of files) {
    const lines = readFileSync(file, "utf8").split(/\r?\n/);
    lines.forEach((line, index) => {
      const attr = cfgAttribute(line);
      if (!attr || cfgOnWindows(attr.expression) !== false) return;
      if (attr.inner) { gated.add(file); return; }
      let text = attr.rest;
      let at = index;
      while (!text || /^#\[/.test(text.trim())) { at += 1; if (at >= lines.length) return; text = lines[at].trim(); }
      const declared = text.match(/^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z0-9_]+)\s*;/);
      if (!declared) return;
      const dir = path.dirname(file);
      gated.add(path.join(dir, `${declared[1]}.rs`));
      gated.add(path.join(dir, declared[1], "mod.rs"));
      gated.add(path.join(dir, declared[1]));
    });
  }
  return gated;
}
const inGated = (file, gated) => {
  for (let current = file; current !== path.dirname(current); current = path.dirname(current)) if (gated.has(current)) return true;
  return false;
};

export function unhiddenSpawns(root = crates) {
  const violations = [];
  for (const crate of readdirSync(root, { withFileTypes: true })) {
    if (!crate.isDirectory()) continue;
    const src = path.join(root, crate.name, "src");
    let files;
    try { files = rustFiles(src); } catch { continue; }
    const gated = gatedPaths(files);
    for (const file of files) {
      const relative = path.relative(root, file).replaceAll("\\", "/");
      if (MACOS_ONLY_FILE.test(relative) || inGated(file, gated)) continue;
      const lines = readFileSync(file, "utf8").split(/\r?\n/);
      const absent = linesAbsentOnWindows(lines);
      lines.forEach((line, index) => {
        if (!SPAWN.test(line) || line.trimStart().startsWith("//") || absent[index]) return;
        // Only the lines that build this command, up to where it launches or is handed off.
        let end = index;
        while (end < Math.min(lines.length - 1, index + 30) && !LAUNCH.test(lines[end])) end += 1;
        if (HIDDEN.test(lines.slice(index, end + 1).join("\n"))) return;
        if (relative === OWNED_COMMAND_CONSTRUCTOR.file && OWNED_COMMAND_CONSTRUCTOR.line.test(line)) return;
        if (lines.slice(Math.max(0, index - 3), index + 1).some((l) => l.includes("// windowless:"))) return;
        violations.push(`${relative}:${index + 1}: ${line.trim()}`);
      });
    }
  }
  return violations;
}

test("crate-source child processes never open a console window on Windows", () => {
  assert.deepEqual(unhiddenSpawns(), [], "add CREATE_NO_WINDOW (creation_flags(0x0800_0000)), spawn through OwnedCommand, or document with `// windowless: <reason>`");
});

test("the guard flags an unhidden spawn & accepts hidden, off-Windows & documented ones", async () => {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const root = mkdtempSync(path.join(tmpdir(), "rk-windowless-"));
  try {
    mkdirSync(path.join(root, "demo", "src"), { recursive: true });
    writeFileSync(path.join(root, "demo", "src", "lib.rs"), [
      "fn bad() { let _ = std::process::Command::new(\"git\").output(); }",
      "fn good() {",
      "    let mut c = std::process::Command::new(\"git\");",
      "    #[cfg(windows)] { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }",
      "    let _ = c.output();",
      "}",
      "#[cfg(target_os = \"macos\")]",
      "fn mac() { let _ = std::process::Command::new(\"open\").status(); }",
      "fn documented() {",
      "    // windowless: handed to OwnedCommand later",
      "    let _c = std::process::Command::new(\"x\");",
      "}",
      "fn owned() { let _ = OwnedCommand::new(\"ffmpeg\"); }",
    ].join("\n"));
    assert.deepEqual(unhiddenSpawns(root), ["demo/src/lib.rs:1: fn bad() { let _ = std::process::Command::new(\"git\").output(); }"]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("cfg expressions: only code certainly absent from a normal Windows build is skipped", () => {
  const cases = { 'unix': false, 'test': false, 'target_os = "macos"': false, 'target_os = "linux"': false, 'not(windows)': false, 'not(target_os = "windows")': false,
    'all(unix, not(target_os = "macos"))': false, 'any(target_os = "macos", target_os = "linux")': false, 'all(test, windows)': false,
    'windows': true, 'target_os = "windows"': true, 'not(test)': true, 'not(unix)': true, 'any(unix, windows)': true, 'any(unix, target_os = "windows")': true,
    'feature = "x"': null, 'debug_assertions': null, 'all(unix, feature = "x")': false, 'any(unix, feature = "x")': null, 'not(feature = "x")': null };
  for (const [expression, expected] of Object.entries(cases)) assert.equal(cfgOnWindows(expression), expected, expression);
});

async function scan(files) {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const root = mkdtempSync(path.join(tmpdir(), "rk-windowless-"));
  try {
    for (const [name, text] of Object.entries(files)) {
      mkdirSync(path.dirname(path.join(root, "demo", "src", name)), { recursive: true });
      writeFileSync(path.join(root, "demo", "src", name), text);
    }
    return unhiddenSpawns(root);
  } finally { rmSync(root, { recursive: true, force: true }); }
}
const BARE = 'let _ = std::process::Command::new("git").output();';

test("spawns inside #[cfg(test)] modules are skipped however deep, and the scan resumes after the module closes", async () => {
  const filler = Array.from({ length: 80 }, (_, i) => `    fn filler_${i}() {}`);
  const found = await scan({ "lib.rs": [
    "pub fn run() {}",
    "#[cfg(test)]", "mod tests {", "    use super::*;", ...filler, `    fn t() { ${BARE} }`, "}",
    `fn after_tests() { ${BARE} }`,
  ].join("\n") });
  // line 1 pub fn, 2-3 attribute + mod, 4 use, 5-84 filler, 85 test fn, 86 closing brace, 87 the spawn.
  assert.deepEqual(found, [`demo/src/lib.rs:87: fn after_tests() { ${BARE} }`]);
});

test("spawns in cfg(unix) / cfg(not(windows)) items, blocks and statements are skipped; the next ungated item is not", async () => {
  const found = await scan({ "lib.rs": [
    "#[cfg(unix)]", "fn unix_only() {", `    ${BARE}`, "}",
    "#[cfg(not(windows))]", "mod posix {", "    pub fn go() {", `        ${BARE}`, "    }", "}",
    "#[cfg(all(unix, not(target_os = \"macos\")))]", `fn linux_only() { ${BARE} }`,
    "fn mixed() {", "    #[cfg(unix)]", "    {", `        ${BARE}`, "    }", "}",
    `fn windows_reachable() { ${BARE} }`,
  ].join("\n") });
  assert.deepEqual(found, [`demo/src/lib.rs:19: fn windows_reachable() { ${BARE} }`]);
});

test("a real Windows-reachable bare spawn is still caught: cfg(windows), cfg(any(unix, windows)), cfg(not(test)), cfg(feature), and no cfg", async () => {
  const found = await scan({ "lib.rs": [
    "#[cfg(windows)]", `fn a() { ${BARE} }`,
    "#[cfg(any(unix, windows))]", `fn b() { ${BARE} }`,
    "#[cfg(not(test))]", `fn c() { ${BARE} }`,
    "#[cfg(feature = \"extra\")]", `fn d() { ${BARE} }`,
    `fn e() { ${BARE} }`,
  ].join("\n") });
  assert.deepEqual(found.map((f) => f.split(":")[1]), ["2", "4", "6", "8", "9"]);
});

test("a cfg(unix) item does not exempt later code within 40 lines (the old nearest-attribute rule did)", async () => {
  const found = await scan({ "lib.rs": ["#[cfg(unix)]", "fn unix_helper() {}", "fn windows_path() {", `    ${BARE}`, "}"].join("\n") });
  assert.deepEqual(found, [`demo/src/lib.rs:4: ${BARE}`]);
});

test("files gated away from Windows are skipped: #![cfg(unix)] inside, #[cfg(unix)] mod x; outside, and their children", async () => {
  const found = await scan({
    "lib.rs": ["#[cfg(unix)]", "mod process_unix;", "#[cfg(not(windows))]", "pub mod posix;", "mod portable;", ""].join("\n"),
    "process_unix.rs": `pub fn go() { ${BARE} }\n`,
    "posix/mod.rs": `pub fn go() { ${BARE} }\n`,
    "posix/nested.rs": `pub fn go() { ${BARE} }\n`,
    "inner.rs": `#![cfg(unix)]\npub fn go() { ${BARE} }\n`,
    "portable.rs": `pub fn go() { ${BARE} }\n`,
  });
  assert.deepEqual(found, [`demo/src/portable.rs:1: pub fn go() { ${BARE} }`]);
});

test("the OwnedCommand constructor exemption is exact: the same line elsewhere is still flagged", async () => {
  const found = await scan({ "owned.rs": "impl X { pub fn new(program: P) -> Self {\n    Self::from_command(Command::new(program))\n} }\n" });
  assert.equal(found.length, 1);
});
