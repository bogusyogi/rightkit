import assert from "node:assert/strict";
import test from "node:test";
import { publicationContent } from "./run-deterministic-scans.mjs";
import { externalInputs } from "../../packages/release/bin/rightkit-gate.mjs";

const files = ["Cargo.toml", "Cargo.lock", "deny.toml", "crates/rightkit-daemon/Cargo.toml", "crates/rightkit-daemon/src/lib.rs", "crates/rightkit-other/Cargo.toml", "crates/rightkit-other/src/lib.rs", "desktop-app-template/src/lib.rs", "vendor/example/src/lib.rs", "shared/api.rs", "ci/consumers/rightkit-daemon/src/lib.rs", "ci/consumers/other/src/lib.rs", "ci/gate.config.json", "scripts/ci/gate.mjs", "packages/release/index.mjs", ".github/workflows/publish.yml"];
const workspace = new Map([["rightkit-daemon", { dir: "crates/rightkit-daemon" }]]);
const select = (extra = {}) => publicationContent({ registry: "crates", units: "rightkit-daemon rightkit-daemon", files, workspace, readBlob: () => "", ...extra });

test("publication content includes full selected units, external sources & shared inputs without unrelated Rust", () => {
  const selected = select({ readBlob: (f) => f.endsWith("src/lib.rs") ? 'include!("../../../shared/api.rs");' : "" });
  assert.deepEqual(selected.scope, { registry: "crates", units: ["rightkit-daemon"] });
  assert.deepEqual(selected.rustUnits, [{ name: "rightkit-daemon", dir: "crates/rightkit-daemon" }]);
  assert.deepEqual(selected.npmUnits, []);
  assert.deepEqual(selected.files, files.filter((f) => !["crates/rightkit-other/src/lib.rs", "desktop-app-template/src/lib.rs", "vendor/example/src/lib.rs", "ci/consumers/other/src/lib.rs"].includes(f)));
});

test("publication content refuses missing scope, unknown units & computed external includes", () => {
  assert.throws(() => select({ registry: undefined }), /PUBLISH_REGISTRY/);
  assert.throws(() => select({ units: "rightkit-other" }), /not publishable/);
  assert.throws(() => select({ readBlob: () => "include!(computed_path());" }), /inputs unresolved/);
});

test("runtime security paths do not widen publication scope or reject it; cache defaults remain conservative", () => {
  const readBlob = (f) => f === "crates/rightkit-daemon/src/lib.rs" ? 'let attacks = ["../../../../..", "../../../../vendor", "../../../crates"]; include!("../../../shared/api.rs");' : "";
  const selected = select({ readBlob });
  assert(selected.files.includes("shared/api.rs"));
  assert(!selected.files.includes("crates/rightkit-other/src/lib.rs"));
  assert(!selected.files.includes("vendor/example/src/lib.rs"));
  const reach = externalInputs({ dir: "crates/rightkit-daemon", files: ["crates/rightkit-daemon/src/lib.rs"], readBlob, exists: (p) => files.some((f) => f === p || f.startsWith(`${p}/`)) });
  assert.match(reach.unresolved, /leaves the repository/);
});

test("compile-input mode still refuses build-script file reads outside repository", () => {
  const build = "crates/rightkit-daemon/build.rs";
  assert.throws(() => select({ files: [...files, build], readBlob: (f) => f === build ? 'let bytes = std::fs::read("../../../../outside");' : "" }), /inputs unresolved/);
});

test("npm selection uses published package identity & includes whole package", () => {
  const selected = select({ registry: "npm", units: "@rightkit/logs", workspace: new Map([["@rightkit/logs", { dir: "packages/logs" }]]), files: [...files, "packages/logs/package.json", "packages/logs/src/index.ts", "packages/other/src/index.ts"] });
  assert.deepEqual(selected.npmUnits, [{ name: "logs", dir: "packages/logs" }]);
  assert.deepEqual(selected.rustUnits, []);
  assert(selected.files.includes("packages/logs/src/index.ts"));
  assert(!selected.files.includes("packages/other/src/index.ts"));
});
