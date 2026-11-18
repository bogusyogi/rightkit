import { test } from "node:test";
import assert from "node:assert/strict";
import { cargoManifestsOfWorkspace } from "./bin/rightkit-gate.mjs";

test("a workspace's advisory rows read its own manifests and those its path dependencies reach, not every Cargo.toml (NN04)", () => {
  // The witness fixture's layout (2026-11-17), plus a path dependency across workspaces.
  const files = {
    "rk/Cargo.toml": "[workspace]\nmembers = [\"crates/*\"]\n",
    "rk/crates/rk-core/Cargo.toml": "[package]\nname = \"rk-core\"\n",
    "rk/crates/rk-leaf/Cargo.toml": "[package]\nname = \"rk-leaf\"\n[dependencies]\nrk-core = { path = \"../rk-core\" }\n",
    "app/engine/Cargo.toml": "[package]\nname = \"witness-engine\"\n",
    "app/src-tauri/Cargo.toml": "[package]\nname = \"witness_app\"\n[dependencies]\nwitness-engine = { path = \"../engine\", version = \"0.1\" }\n",
    "crates/rightkit-a/Cargo.toml": "[package]\nname = \"rightkit-a\"\n",
    "crates/rightkit-b/Cargo.toml": "[package]\nname = \"rightkit-b\"\n[dependencies.rightkit-a]\npath = \"../rightkit-a\"\n",
  };
  const all = [...Object.keys(files), "README.md"];
  const of = (dir) => cargoManifestsOfWorkspace({ dir, all, readBlob: (f) => files[f] });
  assert.deepEqual(of("rk"), ["rk/Cargo.toml", "rk/crates/rk-core/Cargo.toml", "rk/crates/rk-leaf/Cargo.toml"], "an rk edit is the only thing that changes rk's rows");
  assert.deepEqual(of("app/engine"), ["app/engine/Cargo.toml"]);
  assert.deepEqual(of("app/src-tauri"), ["app/engine/Cargo.toml", "app/src-tauri/Cargo.toml"], "a path dependency outside the workspace counts");
  assert.deepEqual(of("crates/rightkit-b"), ["crates/rightkit-a/Cargo.toml", "crates/rightkit-b/Cargo.toml"], "a [dependencies.x] table with path = counts too");
  assert.deepEqual(of(".").length, 7, "the root workspace reads every manifest");
  // A path that leaves the repository is ignored; target paths ([lib], [[bin]]) are not dependencies.
  assert.deepEqual(cargoManifestsOfWorkspace({ dir: "x", all: ["x/Cargo.toml"], readBlob: () => "[dependencies]\na = { path = \"../../outside\" }\n" }), ["x/Cargo.toml"]);
  assert.deepEqual(cargoManifestsOfWorkspace({ dir: "x", all: ["x/Cargo.toml", "y/Cargo.toml"], readBlob: () => "[lib]\npath = \"src/lib.rs\"\n[[bin]]\nname = \"b\"\npath = 'src/main.rs'\n" }), ["x/Cargo.toml"]);
});

// Each expectation is what Cargo resolves for that manifest, written from Cargo's rules (and, for astra62's cases, checked by astra62 with Python's tomllib), not from this reader.
test("NN04: every TOML spelling of a path is followed; inheritance reaches the workspace root; whenever it cannot tell, every Cargo.toml (astra62 reviews 2-3)", () => {
  const all = ["x/Cargo.toml", "y/Cargo.toml", "z/Cargo.toml"];
  const of = (files, extra = [], dir = "x") => cargoManifestsOfWorkspace({ dir, all: [...all, ...extra], readBlob: (f) => { if (!(f in files)) throw new Error(`ENOENT ${f}`); return files[f]; } });
  const plain = { "y/Cargo.toml": "[package]\n", "z/Cargo.toml": "[package]\n" };
  const xy = ["x/Cargo.toml", "y/Cargo.toml"];
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\ny = { path = '../y' }\n" }), xy, "single quotes");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\ny = { \"path\" = \"../y\" }\n" }), xy, "quoted key");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\ny = { \"\\u0070ath\" = \"../y\" }\n" }), xy, "Unicode-escaped key");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\ny.path = \"../y\"\ny.version = \"1\"\n" }), xy, "dotted key");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[\"dependencies\".'y']\npath = \"\"\"\n../y\"\"\"\n" }), xy, "quoted table, multi-line string");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[target.'cfg(windows)'.dependencies]\ny = { path = \"../y\" }\n" }), xy, "target-specific");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\n# y = { path = \"../y\" }\n" }), ["x/Cargo.toml"], "a comment is not a dependency");
  // Inheritance: a member's `y.workspace = true` reads the root's [workspace.dependencies], which names y by path.
  const root = "[workspace]\nmembers = [\"x\"]\n[workspace.dependencies]\ny = { path = \"y\" }\n";
  assert.deepEqual(cargoManifestsOfWorkspace({ dir: "x", all: ["Cargo.toml", ...all], readBlob: (f) => ({ ...plain, "Cargo.toml": root, "x/Cargo.toml": "[package]\nname = \"x\"\n[dependencies]\ny.workspace = true\n" })[f] }), ["Cargo.toml", "x/Cargo.toml", "y/Cargo.toml"], "inherited sibling dependency");
  assert.deepEqual(of({ ...plain, "z/Cargo.toml": "[workspace]\n[workspace.dependencies]\ny = { path = \"../y\" }\n", "x/Cargo.toml": "[package]\nworkspace = \"../z\"\nversion.workspace = true\n[dependencies]\ny = { workspace = true }\n" }), ["x/Cargo.toml", "y/Cargo.toml", "z/Cargo.toml"], "package.workspace names the root");
  // Cannot tell: every one.
  assert.deepEqual(of({ ...plain }), all, "unreadable");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies\ny = 1\n" }), all, "not TOML");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\ny = { path = \"..\\\\y\" }\n" }), all, "a backslash path");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[package]\nversion.workspace = true\n" }), all, "inherits from a root it cannot find");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[workspace]\nmembers = [\n  \"a\",\n  \"../z\",\n]\n" }), all, "a member outside the workspace");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[package]\n", ".cargo/config.toml": "\"paths\" = [\"y\"]\n" }, [".cargo/config.toml"]), [...all].sort(), "a paths override");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[package]\n", "x/.cargo/config.toml": "[patch.crates-io]\ny = { path = \"../y\" }\n" }, ["x/.cargo/config.toml"]), [...all].sort(), "a patch in Cargo configuration");
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[package]\n", "x/.cargo/config.toml": "[build]\njobs = 2\n" }, ["x/.cargo/config.toml"]), ["x/Cargo.toml"], "an unrelated configuration changes nothing");
});
