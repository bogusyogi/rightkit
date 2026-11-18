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
  // A path that leaves the repository, or names no manifest, is ignored; an unreadable manifest is kept but not followed.
  assert.deepEqual(cargoManifestsOfWorkspace({ dir: "x", all: ["x/Cargo.toml"], readBlob: () => "[dependencies]\na = { path = \"../../outside\" }\n" }), ["x/Cargo.toml"]);
  assert.deepEqual(cargoManifestsOfWorkspace({ dir: "x", all: ["x/Cargo.toml", "y/Cargo.toml"], readBlob: () => { throw new Error("no"); } }), ["x/Cargo.toml"]);
});
