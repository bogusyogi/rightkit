import { test } from "node:test";
import assert from "node:assert/strict";
import { externalInputs } from "./bin/rightkit-gate.mjs";

// A small repository: path -> text. `exists` answers for files and for the directories above them, as the gate's git lookup does.
const repo = (files) => {
  const dirs = new Set(["."]);
  for (const f of Object.keys(files)) { const parts = f.split("/"); for (let i = 1; i < parts.length; i += 1) dirs.add(parts.slice(0, i).join("/")); }
  return { readBlob: (f) => { if (!(f in files)) throw new Error("absent"); return files[f]; }, exists: (p) => p in files || dirs.has(p), list: (dir) => Object.keys(files).filter((f) => f.startsWith(`${dir}/`)) };
};
const reach = (files, dir) => { const r = repo(files); return externalInputs({ dir, files: r.list(dir), readBlob: r.readBlob, exists: r.exists }); };

test("a literal that only walks up to a directory the crate is inside is not an input: string handling does not key a crate on the whole repository", () => {
  // The real lines (RustRight, 2026-11-17): rustright-policy/src/input_closure.rs and workspace_scan.rs, rustright-config/src/paths.rs.
  const files = {
    "crates/policy/src/lib.rs": 'pub fn escapes(clean: &str) -> bool { clean == ".." || clean.starts_with("../") || clean.starts_with("..\\\\") }\npub fn up(out: &mut Vec<&str>) { out.push(".."); }\npub fn two() -> &\'static str { "../.." }\n',
    "crates/policy/Cargo.toml": "[package]\nname = \"policy\"\n",
    "crates/other/src/lib.rs": "pub fn f() {}\n",
    "docs/contract.json": "{}",
  };
  const r = reach(files, "crates/policy");
  assert.deepEqual(r.paths, [], "neither crates/ nor the repository root is an input");
  assert.equal(r.unresolved, null);
  assert.equal(r.leavesCrate, null, "library code, no test");
});

test("a named file outside the crate is still an input, and a file reached through include_str! still is", () => {
  const files = {
    "crates/policy/src/lib.rs": 'pub const C: &str = include_str!("../../../docs/contract.json");\npub fn p() -> &\'static str { "../other/src/lib.rs" }\n',
    "crates/other/src/lib.rs": "pub fn f() {}\n",
    "docs/contract.json": "{}",
  };
  assert.deepEqual(reach(files, "crates/policy").paths, ["crates/other/src/lib.rs", "docs/contract.json"]);
});

test("test code that walks up without naming a file still keys its tests on the whole tree; a string test or a comparison does not", () => {
  const lib = (testBody) => ({ "crates/policy/src/lib.rs": `pub fn f() {}\n#[cfg(test)]\nmod tests {\n${testBody}\n}\n`, "docs/contract.json": "{}" });
  for (const walk of ['let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");', 'let root = dir.join("..").join("..");', 'let root = dir.join("../../");', 'let up = "..\\\\..";']) {
    assert.equal(reach(lib(walk), "crates/policy").leavesCrate, "crates/policy/src/lib.rs", walk);
    assert.deepEqual(reach(lib(walk), "crates/policy").paths, [], walk);
  }
  for (const plain of ['assert!(!clean.starts_with("../"));', 'assert!(name != "..");', 'assert!(part == "..");', 'assert!(!text.contains(".."));', 'assert!(a.strip_prefix("../").is_none());']) {
    assert.equal(reach(lib(plain), "crates/policy").leavesCrate, null, plain);
  }
});
