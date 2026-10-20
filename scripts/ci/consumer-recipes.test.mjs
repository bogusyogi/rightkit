import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { cargoRecipe, nativeFixture } from "./consumer-recipes.mjs";
const unit = { name: "rightkit-logs", version: "0.3.4" };
const committedRoot = fileURLToPath(new URL("../../", import.meta.url));
const manifest = fs.readFileSync(path.join(committedRoot, "ci/consumers/rightkit-logs/Cargo.toml"), "utf8");
function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-pk-recipe-"))); t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const dir = path.join(root, "ci/consumers/rightkit-logs"); fs.mkdirSync(path.dirname(dir), { recursive: true });
  fs.cpSync(path.join(committedRoot, "ci/consumers/rightkit-logs"), dir, { recursive: true });
  fs.copyFileSync(path.join(committedRoot, "ci/consumer-recipes.json"), path.join(root, "ci/consumer-recipes.json"));
  return { root, dir };
}
test("PK identity binds executable source, every file, exact dependency & selected feature prescription", t => {
  const { root, dir } = fixture(t); const recipe = cargoRecipe({ root, unit });
  assert.equal(recipe.evidence.id, "ci/consumers/rightkit-logs/src/main.rs");
  assert.equal(recipe.evidence.pkId, "PK11");
  assert.deepEqual(recipe.evidence.dependency, { name: unit.name, version: "=0.3.4", defaultFeatures: false, features: [] });
  assert.equal(Object.keys(recipe.evidence.files).length, 2);
  fs.appendFileSync(path.join(dir, "src/main.rs"), "// changed API prescription\n"); assert.throws(() => cargoRecipe({ root, unit }), /differs from reviewed PK11 inventory/);
});

test("reviewed inventory admits only existing locked PK01 & PK11 examples", () => {
  const inventory = JSON.parse(fs.readFileSync(path.join(committedRoot, "ci/consumer-recipes.json"), "utf8"));
  assert.deepEqual(Object.keys(inventory.cargo).sort(), ["rightkit-fs", "rightkit-logs"]);
  for (const [name, version, pkId] of [["rightkit-fs", "0.4.6", "PK01"], ["rightkit-logs", "0.3.4", "PK11"]]) {
    const { evidence } = cargoRecipe({ root: committedRoot, unit: { name, version } });
    assert.equal(evidence.pkId, pkId); assert.deepEqual(evidence.files, inventory.cargo[name].files);
  }
});
test("missing PK example, empty generic smoke, redirects & unsafe recipe names refuse", t => {
  const { root, dir } = fixture(t);
  assert.throws(() => cargoRecipe({ root, unit: { name: "rightkit-missing", version: "1.0.0" } }), /add reviewed exact-version public API recipe/);
  assert.throws(() => cargoRecipe({ root, unit: { name: "../escape", version: "1.0.0" } }), /unsafe/);
  for (const source of ["fn main() {}\n", "use rightkit_logs::KnownSecrets; fn main() {}\n", "// use rightkit_logs::KnownSecrets;\nfn main() {}\n"]) {
    fs.writeFileSync(path.join(dir, "src/main.rs"), source); assert.throws(() => cargoRecipe({ root, unit }), /differs from reviewed PK11 inventory/);
  }
  const inventoryPath = path.join(root, "ci/consumer-recipes.json"); const inventory = fs.readFileSync(inventoryPath, "utf8");
  fs.writeFileSync(inventoryPath, '{"schema":1,"cargo":{}}'); assert.throws(() => cargoRecipe({ root, unit }), /missing reviewed PK inventory/); fs.writeFileSync(inventoryPath, inventory);
  for (const extra of ['\n[patch.crates-io]\nx={path="../x"}\n', '\nother={git="https://foreign.test"}\n', '\nother={workspace=true}\n', '\nother={version="1",registry="foreign"}\n']) { fs.writeFileSync(path.join(dir, "Cargo.toml"), manifest + extra); assert.throws(() => cargoRecipe({ root, unit })); }
  fs.writeFileSync(path.join(dir, "Cargo.toml"), manifest); fs.mkdirSync(path.join(dir, ".cargo")); assert.throws(() => cargoRecipe({ root, unit }), /hidden config/);
});
test("FS PK example requires real existing native fixtures; ordinary scratch directories cannot qualify", t => {
  const { root } = fixture(t); const fsUnit = { name: "rightkit-fs", version: "0.4.6" };
  assert.equal(nativeFixture({ unit, env: {} }), null);
  assert.throws(() => nativeFixture({ unit: fsUnit, env: { RUNNER_TEMP: root }, platform: "win32" }), /native fixture setup.*state.json missing/);
  const scratch = path.join(root, "rk-fs-volume-test"); fs.mkdirSync(scratch); fs.mkdirSync(path.join(scratch, "other"));
  fs.writeFileSync(path.join(root, "rk-fs-native-state.json"), JSON.stringify({ schema: 1, platform: "darwin", scratch, other: path.join(scratch, "other"), fat: null }));
  fs.writeFileSync(path.join(scratch, "native-volume-info.txt"), JSON.stringify({ other: "APFS", fat: null }));
  assert.throws(() => nativeFixture({ unit: fsUnit, env: { RUNNER_TEMP: root }, platform: "darwin" }), /distinct mounted volume missing/);
});
