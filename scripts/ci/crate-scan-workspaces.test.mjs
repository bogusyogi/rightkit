import test from "node:test";
import assert from "node:assert/strict";
import { owningWorkspace, scanRustUnits, scanUnitName, scanWorkspaceDirs } from "./crate-scan-workspaces.mjs";

// A fake checkout: path -> text. Only manifests and locks matter.
const files = {
  "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\nexclude = ["crates/rightkit-mesh-ffi"]\n',
  "Cargo.lock": "# lock\n",
  "crates/rightkit-asr/Cargo.toml": '[package]\nname = "rightkit-asr"\n',
  "crates/rightkit-dictation-engine/Cargo.toml": '[package]\nname = "rightkit-dictation-engine"\n',
  "crates/rightkit-mesh-ffi/Cargo.toml": '[package]\nname = "rightkit-mesh-ffi"\n\n[workspace]\n',
};
const io = { exists: (f) => Object.hasOwn(files, f.replace(/^\//, "")), readFile: (f) => files[f.replace(/^\//, "")] };
const root = "";
const crates = [
  { name: "rightkit-asr", dir: "crates/rightkit-asr" },
  { name: "rightkit-dictation-engine", dir: "crates/rightkit-dictation-engine" },
  { name: "rightkit-mesh-ffi", dir: "crates/rightkit-mesh-ffi" },
];

test("a crate of the root workspace is scanned in the root", () => {
  assert.equal(owningWorkspace(root, "crates/rightkit-asr", io), ".");
});

test("a crate that declares its own [workspace] keeps its own directory", () => {
  assert.equal(owningWorkspace(root, "crates/rightkit-mesh-ffi", io), "crates/rightkit-mesh-ffi");
});

test("a crate holding its own Cargo.lock owns its workspace", () => {
  const withLock = { ...io, exists: (f) => io.exists(f) || f === "crates/rightkit-asr/Cargo.lock" };
  assert.equal(owningWorkspace(root, "crates/rightkit-asr", withLock), "crates/rightkit-asr");
});

test("two root crates yield one workspace entry; the own-workspace crate a second", () => {
  const units = scanRustUnits(root, crates, io);
  assert.deepEqual(units.map((u) => [u.name, u.cargoDir]), [
    ["rightkit-asr", "."], ["rightkit-dictation-engine", "."], ["rightkit-mesh-ffi", "crates/rightkit-mesh-ffi"],
  ]);
  assert.deepEqual(scanWorkspaceDirs(units), [".", "crates/rightkit-mesh-ffi"]);
  assert.deepEqual(scanWorkspaceDirs(units.slice(0, 2)), ["."]);
});

test("scan unit names match rightkit-gate: plain for the root and a crate's own directory, suffixed below", () => {
  assert.equal(scanUnitName(".", "a"), "a");
  assert.equal(scanUnitName("crates/a", "a"), "a");
  assert.equal(scanUnitName("apps/x", "a"), "apps/x:a");
});
