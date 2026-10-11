import assert from "node:assert/strict";
import test from "node:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { findWorkspaceRoot, lockClosure, pathDependencyDirs, readCargoPackage } from "./crate-graph.mjs";

function workspace(t, files) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-crate-graph-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const [rel, text] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    writeFileSync(path.join(root, rel), text);
  }
  return root;
}

test("readCargoPackage parses name and version, and resolves version.workspace = true", (t) => {
  const root = workspace(t, {
    "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nversion = "3.1.4"\n',
    "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.2.0"\n',
    "crates/b/Cargo.toml": '[package]\nname = "b"\nversion.workspace = true\n',
    "crates/c/Cargo.toml": '[package]\nname = "c"\nversion = { workspace = true }\n',
    "crates/d/Cargo.toml": '[package]\nname = "d"\n',
  });
  assert.deepEqual([readCargoPackage(path.join(root, "crates/a")).name, readCargoPackage(path.join(root, "crates/a")).version], ["a", "0.2.0"]);
  assert.equal(readCargoPackage(path.join(root, "crates/b")).version, "3.1.4");
  assert.equal(readCargoPackage(path.join(root, "crates/c")).version, "3.1.4");
  assert.equal(readCargoPackage(path.join(root, "crates/d")).version, null);
  assert.equal(findWorkspaceRoot(path.join(root, "crates/a")), root);
});

test("pathDependencyDirs follows path dependencies transitively, including workspace-inherited and target tables", (t) => {
  const root = workspace(t, {
    "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n[workspace.dependencies]\ninherited = { path = "crates/inherited" }\nserde = "1"\n',
    "crates/svc/Cargo.toml": '[package]\nname = "svc"\nversion = "1.0.0"\n[dependencies]\nmid = { path = "../mid", version = "1" }\nserde.workspace = true\ninherited.workspace = true\n[target.\'cfg(windows)\'.dependencies]\nwin = { path = "../win" }\n[dev-dependencies]\ndevonly = { path = "../devonly" }\n',
    "crates/mid/Cargo.toml": '[package]\nname = "mid"\nversion = "1.0.0"\n[dependencies]\nleaf = { path = "../leaf" }\n[dev-dependencies]\nmid-dev = { path = "../middev" }\n',
    "crates/leaf/Cargo.toml": '[package]\nname = "leaf"\nversion = "1.0.0"\n[dependencies]\nsvc = { path = "../svc" }\n',
    "crates/inherited/Cargo.toml": '[package]\nname = "inherited"\nversion = "1.0.0"\n',
    "crates/win/Cargo.toml": '[package]\nname = "win"\nversion = "1.0.0"\n',
    "crates/devonly/Cargo.toml": '[package]\nname = "devonly"\nversion = "1.0.0"\n',
    "crates/middev/Cargo.toml": '[package]\nname = "middev"\nversion = "1.0.0"\n',
    "crates/unrelated/Cargo.toml": '[package]\nname = "unrelated"\nversion = "1.0.0"\n',
  });
  const names = pathDependencyDirs(path.join(root, "crates/svc")).map((d) => path.basename(d));
  // The root crate's dev-dependencies count; a dependency's dev-dependencies do not; cycles terminate.
  assert.deepEqual(names, ["devonly", "inherited", "leaf", "mid", "win"]);
});

test("lockClosure covers the crate, workspace crates and external packages it reaches, and nothing else", () => {
  const lock = (serdeChecksum, tokioVersion) => `version = 4
[[package]]
name = "svc"
version = "0.2.0"
dependencies = ["dep-b", "serde 1.0.0"]
[[package]]
name = "dep-b"
version = "0.1.0"
dependencies = ["serde 1.0.0"]
[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "${serdeChecksum}"
[[package]]
name = "other"
version = "0.1.0"
dependencies = ["tokio"]
[[package]]
name = "tokio"
version = "${tokioVersion}"
source = "registry+https://github.com/rust-lang/crates.io-index"
`;
  const base = JSON.stringify(lockClosure(lock("aa", "1.0.0"), "svc"));
  assert.deepEqual(lockClosure(lock("aa", "1.0.0"), "svc").map((p) => p.name), ["dep-b", "serde", "svc"]);
  assert.equal(JSON.stringify(lockClosure(lock("aa", "9.9.9"), "svc")), base, "a package outside the graph does not matter");
  assert.notEqual(JSON.stringify(lockClosure(lock("bb", "1.0.0"), "svc")), base, "a changed checksum inside the graph does");
  assert.deepEqual(lockClosure(lock("aa", "1.0.0"), "missing"), []);
});
