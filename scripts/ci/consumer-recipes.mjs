// Shared identity for committed PK examples. No generated/default consumer can qualify.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const need = (ok, why) => { if (!ok) throw new Error(`registry consumer recipe refused: ${why}`); };
const within = (base, file) => { const rel = path.relative(base, file); return rel && !path.isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${path.sep}`); };

export function assertRegistryManifest(text, unit) {
  need(!/^\s*\[(?:patch|replace)(?:[.\]])/m.test(text) && !/\b(?:path|git|workspace|registry|registry-index)\s*=/.test(text), "registry consumer must not contain path/git/workspace dependencies, foreign registries or patches");
  need(/^rightkit-[a-z0-9][a-z0-9-]*$/.test(unit.name), "invalid consumer crate name");
  const pin = new RegExp(`^${unit.name}\\s*=\\s*\\{[^\\n]*version\\s*=\\s*"=${unit.version.replaceAll(".", "\\.")}"[^\\n]*\\}`, "m");
  need(pin.test(text), `consumer must pin ${unit.name} =${unit.version}`);
}

export function cargoRecipe({ root, unit }) {
  need(/^rightkit-[a-z0-9][a-z0-9-]*$/.test(unit.name), "unsafe recipe name");
  const base = path.resolve(root, "ci/consumers"); const directory = path.join(base, unit.name);
  need(fs.existsSync(directory), `missing committed PK example ci/consumers/${unit.name}/Cargo.toml & src/main.rs; add reviewed exact-version public API recipe before qualification`);
  need(within(fs.realpathSync(base), fs.realpathSync(directory)), "recipe escapes committed directory");
  const files = {};
  function visit(dir) {
    need(!fs.lstatSync(dir).isSymbolicLink(), "recipe symlink");
    for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
      need(!ent.isSymbolicLink() && !ent.name.startsWith(".") && !["target", "Cargo.lock", "node_modules"].includes(ent.name), "recipe contains symlink, hidden config or generated output");
      const full = path.join(dir, ent.name);
      if (ent.isDirectory()) visit(full);
      else { need(ent.isFile(), "recipe special file"); files[path.relative(directory, full).replaceAll("\\", "/")] = hash(fs.readFileSync(full)); }
    }
  }
  visit(directory);
  need(files["Cargo.toml"] && files["src/main.rs"], "committed Cargo manifest & executable PK example required");
  const manifest = fs.readFileSync(path.join(directory, "Cargo.toml"), "utf8"); assertRegistryManifest(manifest, unit);
  const inventoryFile = path.resolve(root, "ci/consumer-recipes.json");
  need(fs.existsSync(inventoryFile) && !fs.lstatSync(inventoryFile).isSymbolicLink(), "reviewed ci/consumer-recipes.json inventory required");
  const inventory = JSON.parse(fs.readFileSync(inventoryFile, "utf8"));
  const reviewed = inventory.schema === 1 && inventory.cargo?.[unit.name];
  const id = `ci/consumers/${unit.name}/src/main.rs`;
  need(reviewed && /^PK\d{2}$/.test(reviewed.pkId) && reviewed.id === id, `missing reviewed PK inventory entry for ${unit.name}; review recipe & inventory before qualification`);
  const sortedFiles = Object.fromEntries(Object.entries(files).sort(([a], [b]) => a.localeCompare(b)));
  const reviewedFiles = Object.fromEntries(Object.entries(reviewed.files ?? {}).sort(([a], [b]) => a.localeCompare(b)));
  need(JSON.stringify(sortedFiles) === JSON.stringify(reviewedFiles), `recipe differs from reviewed ${reviewed.pkId} inventory; review exact full file hashes before qualification`);
  const dependency = manifest.split(/\r?\n/).find(line => line.startsWith(`${unit.name} `));
  need(dependency, "PK dependency must use reviewed single-line exact-version declaration");
  const selectedFeatures = dependency.match(/\bfeatures\s*=\s*\[([^\]]*)\]/)?.[1] ?? "";
  const features = [...selectedFeatures.matchAll(/"([A-Za-z0-9_+/-]+)"/g)].map(match => match[1]);
  return { directory, manifest, dependency, evidence: { id, pkId: reviewed.pkId, sourceCommit: null, files: sortedFiles, sha256: hash(JSON.stringify(sortedFiles)), manifestSha256: files["Cargo.toml"], exampleSha256: files["src/main.rs"], dependency: { name: unit.name, version: `=${unit.version}`, defaultFeatures: !/default-features\s*=\s*false/.test(dependency), features } } };
}

export function nativeFixture({ unit, env, platform = process.platform }) {
  if (unit.name !== "rightkit-fs") return null;
  const fail = why => need(false, `rightkit-fs requires existing native fixture setup (fs-native-fixtures.mjs setup + always-run cleanup): ${why}`);
  if (!env.RUNNER_TEMP || !["win32", "darwin"].includes(platform)) fail("native RUNNER_TEMP required");
  const state = path.join(env.RUNNER_TEMP, "rk-fs-native-state.json");
  if (!fs.existsSync(state)) fail("rk-fs-native-state.json missing; generic source build cannot replace mounted-volume PK example");
  const bytes = fs.readFileSync(state); const plan = JSON.parse(bytes);
  const temp = fs.realpathSync(env.RUNNER_TEMP);
  if (plan.schema !== 1 || plan.platform !== platform || !path.isAbsolute(plan.scratch ?? "") || !within(temp, plan.scratch) || !path.basename(plan.scratch).startsWith("rk-fs-volume-") || plan.other !== path.join(plan.scratch, "other") || plan.fat !== (platform === "win32" ? path.join(plan.scratch, "fat") : null)) fail("foreign fixture plan");
  const infoFile = path.join(plan.scratch, "native-volume-info.txt");
  if (!fs.existsSync(infoFile)) fail("completed native filesystem setup receipt missing");
  const infoBytes = fs.readFileSync(infoFile); const info = JSON.parse(infoBytes);
  if (!(platform === "win32" ? /NTFS/i.test(info.other) && /FAT32/i.test(info.fat) : /APFS/i.test(info.other))) fail("required native filesystem identity missing");
  for (const mount of [plan.other, plan.fat].filter(Boolean)) if (!fs.existsSync(mount) || fs.statSync(mount).dev === fs.statSync(temp).dev) fail("distinct mounted volume missing");
  return { stateSha256: hash(bytes), volumeInfoSha256: hash(infoBytes), platform, scratch: plan.scratch, other: plan.other, fat: plan.fat };
}
