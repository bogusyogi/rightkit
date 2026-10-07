import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

// GUI apps & background services built on RightKit must never flash a console window on Windows.
// A console-subsystem child of a windowless parent gets a fresh visible console unless its spawner
// passes CREATE_NO_WINDOW, so every crate-source `std::process::Command::new` must, within the
// lines that build it, either hide the window or be compiled only off Windows. `OwnedCommand`
// (rightkit-process) hides by default. An explicit `// windowless: <reason>` comment just above a
// spawn documents a site that hides it elsewhere (e.g. hands the Command to OwnedCommand).
const crates = path.resolve(import.meta.dirname, "../../crates");
const SPAWN = /(?<![A-Za-z_])Command::new\(/;
const HIDDEN = /creation_flags|CREATE_NO_WINDOW|\bhidden\(|hide_background_process|spawn_hidden_logged|OwnedCommand::from_command|windows_hide/;
const NON_WINDOWS_CFG = /#\[cfg\((?:target_os = "macos"|unix|all\(unix[^\]]*|not\(windows\)|not\(target_os = "windows"\)|not\(any\(target_os = "macos", windows\)\)|test)\)\]/;
const ANY_CFG = /#\[cfg\(/;
const LAUNCH = /\.(?:spawn|output|status)\(|from_command\(|spawn_hidden_logged\(|hide_background_process\(|\bfinish\(/;
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

export function unhiddenSpawns(root = crates) {
  const violations = [];
  for (const crate of readdirSync(root, { withFileTypes: true })) {
    if (!crate.isDirectory()) continue;
    const src = path.join(root, crate.name, "src");
    let files;
    try { files = rustFiles(src); } catch { continue; }
    for (const file of files) {
      const relative = path.relative(root, file).replaceAll("\\", "/");
      if (MACOS_ONLY_FILE.test(relative)) continue;
      const lines = readFileSync(file, "utf8").split(/\r?\n/);
      lines.forEach((line, index) => {
        if (!SPAWN.test(line) || line.trimStart().startsWith("//")) return;
        // Only the lines that build this command, up to where it launches or is handed off.
        let end = index;
        while (end < Math.min(lines.length - 1, index + 30) && !LAUNCH.test(lines[end])) end += 1;
        if (HIDDEN.test(lines.slice(index, end + 1).join("\n"))) return;
        if (lines.slice(Math.max(0, index - 3), index + 1).some((l) => l.includes("// windowless:"))) return;
        // Nearest cfg attribute above (same item or enclosing block/module) decides the platform.
        for (let above = index; above >= Math.max(0, index - 40); above -= 1) {
          if (ANY_CFG.test(lines[above])) {
            if (NON_WINDOWS_CFG.test(lines[above])) return;
            break;
          }
        }
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
