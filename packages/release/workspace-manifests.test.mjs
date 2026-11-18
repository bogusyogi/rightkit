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
  // A path that leaves the repository, or names no manifest, is ignored.
  assert.deepEqual(cargoManifestsOfWorkspace({ dir: "x", all: ["x/Cargo.toml"], readBlob: () => "[dependencies]\na = { path = \"../../outside\" }\n" }), ["x/Cargo.toml"]);
  // Target paths ([lib], [[bin]]) are parsed and name no manifest.
  assert.deepEqual(cargoManifestsOfWorkspace({ dir: "x", all: ["x/Cargo.toml", "y/Cargo.toml"], readBlob: () => "[lib]\npath = \"src/lib.rs\"\n[[bin]]\nname = \"b\"\npath = 'src/main.rs'\n" }), ["x/Cargo.toml"]);
});

test("NN04: whenever it cannot be sure, a workspace keys on every Cargo.toml of the commit (GC-A19), never fewer (astra62 review 2026-11-18)", () => {
  const all = ["x/Cargo.toml", "y/Cargo.toml", "z/Cargo.toml"];
  const of = (files, extra = []) => cargoManifestsOfWorkspace({ dir: "x", all: [...all, ...extra], readBlob: (f) => { if (!(f in files)) throw new Error(`ENOENT ${f}`); return files[f]; } });
  const plain = { "y/Cargo.toml": "[package]\n", "z/Cargo.toml": "[package]\n" };
  // A single-quoted path dependency is followed.
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\ny = { path = '../y' }\n" }), ["x/Cargo.toml", "y/Cargo.toml"]);
  // An unreadable manifest: every one.
  assert.deepEqual(of({ ...plain }), all);
  // A path it cannot parse (escaped or multi-line string): every one.
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies]\ny = { path = \"..\\\\y\" }\n" }), all);
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[dependencies.y]\npath = \"\"\"\n../y\"\"\"\n" }), all);
  // A member outside the workspace directory, on one line or several: every one.
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[workspace]\nmembers = [\"../y\"]\n" }), all);
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[workspace]\nmembers = [\n  \"a\",\n  \"../z\",\n]\n" }), all);
  // A Cargo configuration of the workspace or an ancestor that overrides paths: every one; one without paths changes nothing.
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[package]\n", ".cargo/config.toml": "paths = [\"y\"]\n" }, [".cargo/config.toml"]), [...all].sort());
  assert.deepEqual(of({ ...plain, "x/Cargo.toml": "[package]\n", "x/.cargo/config.toml": "[build]\njobs = 2\n" }, ["x/.cargo/config.toml"]), ["x/Cargo.toml"]);
});
