import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { cargoRecipe, nativeFixture, planRuns, parseRuns, executeRuns, cargoRunArgs, NATIVE_HELPER, planNativeHelper, stageNativeHelper, nativeHelperExecute } from "./consumer-recipes.mjs";
const unit = { name: "rightkit-logs", version: "0.3.5" };
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
  assert.deepEqual(recipe.evidence.dependency, { name: unit.name, version: "=0.3.5", defaultFeatures: false, features: [] });
  assert.equal(Object.keys(recipe.evidence.files).length, 2);
  fs.appendFileSync(path.join(dir, "src/main.rs"), "// changed API prescription\n"); assert.throws(() => cargoRecipe({ root, unit }), /differs from reviewed PK11 inventory/);
});

test("reviewed inventory admits only existing locked PK examples", () => {
  const inventory = JSON.parse(fs.readFileSync(path.join(committedRoot, "ci/consumer-recipes.json"), "utf8"));
  assert.deepEqual(Object.keys(inventory.cargo).sort(), ["rightkit-agent-tools", "rightkit-align", "rightkit-android", "rightkit-app-settings", "rightkit-asr", "rightkit-automation", "rightkit-browser", "rightkit-child-process", "rightkit-crash", "rightkit-credentials", "rightkit-daemon", "rightkit-db", "rightkit-device", "rightkit-docs", "rightkit-embed", "rightkit-ffmpeg", "rightkit-fs", "rightkit-fsindex", "rightkit-google-identity", "rightkit-http", "rightkit-job-queue", "rightkit-license", "rightkit-logs", "rightkit-models", "rightkit-motion", "rightkit-oauth", "rightkit-onnx", "rightkit-overlay", "rightkit-overlay-win", "rightkit-ownership", "rightkit-pdf", "rightkit-playback", "rightkit-qa", "rightkit-qa-driver", "rightkit-record", "rightkit-sandbox", "rightkit-sidecar", "rightkit-speech-client", "rightkit-spend", "rightkit-sync-keys", "rightkit-tauri-shell", "rightkit-vault"]);
  for (const [name, version, pkId] of [["rightkit-fs", "0.4.6", "PK01"], ["rightkit-logs", "0.3.5", "PK11"], ["rightkit-speech-client", "0.4.0", "PK15"], ["rightkit-onnx", "0.2.7", "PK18"], ["rightkit-asr", "0.3.0", "PK18"], ["rightkit-qa-driver", "0.7.0", "PK14"], ["rightkit-android", "0.3.0", "PK09"], ["rightkit-google-identity", "0.2.0", "PK10"], ["rightkit-vault", "0.2.0", "PK13"], ["rightkit-docs", "0.1.2", "PK16"], ["rightkit-record", "0.3.3", "PK17"], ["rightkit-align", "0.1.0", "PK21"], ["rightkit-sync-keys", "0.1.0", "PK22"]]) {
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

// ---- recipe runs: reviewed declaration, harness-owned env, prerequisite refusal ----
function withRuns(t, runs) {
  const { root } = fixture(t); const file = path.join(root, "ci/consumer-recipes.json");
  const inventory = JSON.parse(fs.readFileSync(file, "utf8")); inventory.cargo["rightkit-logs"].runs = runs;
  fs.writeFileSync(file, JSON.stringify(inventory)); return root;
}
test("absent runs keep today's single default run with no env", t => {
  const { root } = fixture(t); const recipe = cargoRecipe({ root, unit });
  assert.equal(recipe.runs, null); assert.equal(recipe.evidence.runs, null);
  const plan = planRuns({ runs: recipe.runs, env: {}, scratchRoot: root, platform: "win32" });
  assert.deepEqual(plan.map(r => [r.id, r.args, r.features, r.envNames, r.status, r.values]), [["default", [], [], [], "pending", {}]]);
});
test("declared runs resolve exactly their declared env from the provider table", t => {
  const root = withRuns(t, [{ id: "db", features: ["sqlcipher"], args: ["native-db"], hosts: ["win32", "darwin"], env: { RIGHTKIT_TEST_REAL_USER_STORES: "literal1", RK_CONSUMER_RUN_ROOT: "freshRunRoot" } }, { id: "helper", args: ["native-helper"], hosts: ["darwin"], env: { RK_CONSUMER_HELPER_NAME: "signedHelper", RK_CONSUMER_HELPER_TEAM_ID: "signedHelper" } }]);
  const recipe = cargoRecipe({ root, unit });
  const hostile = { RIGHTKIT_TEST_REAL_USER_STORES: "0", RK_CONSUMER_RUN_ROOT: "/attacker", GITHUB_TOKEN: "secret", RK_R2_SCRATCH: "/attacker" };
  const plan = planRuns({ runs: recipe.runs, env: hostile, scratchRoot: root, platform: "win32" });
  assert.deepEqual(plan.map(r => [r.id, r.status]), [["db", "pending"], ["helper", "not-applicable"]]);
  assert.deepEqual(Object.keys(plan[0].values).sort(), ["RIGHTKIT_TEST_REAL_USER_STORES", "RK_CONSUMER_RUN_ROOT"]);
  assert.equal(plan[0].values.RIGHTKIT_TEST_REAL_USER_STORES, "1");
  assert.ok(isWithinDir(root, plan[0].values.RK_CONSUMER_RUN_ROOT) && plan[0].values.RK_CONSUMER_RUN_ROOT !== "/attacker");
  assert.deepEqual(plan[0].envNames, ["RIGHTKIT_TEST_REAL_USER_STORES", "RK_CONSUMER_RUN_ROOT"]); assert.deepEqual(plan[1].values, {});
  const second = planRuns({ runs: recipe.runs, env: {}, scratchRoot: root, platform: "win32" });
  assert.notEqual(second[0].values.RK_CONSUMER_RUN_ROOT, plan[0].values.RK_CONSUMER_RUN_ROOT);
});
function isWithinDir(parent, child) { const rel = path.relative(parent, child); return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel); }
test("unknown providers, raw inventory values, foreign names & unknown fields are refused", t => {
  const refuse = (run, pattern) => assert.throws(() => cargoRecipe({ root: withRuns(t, [run]), unit }), pattern);
  refuse({ id: "x", env: { RK_X: "attackerProvider" } }, /unknown provider/);
  refuse({ id: "x", env: { RK_CONSUMER_RUN_ROOT: "/tmp/chosen" } }, /unknown provider.*raw values are never accepted/);
  refuse({ id: "x", env: { RIGHTKIT_TEST_REAL_USER_STORES: "1" } }, /unknown provider/);
  refuse({ id: "x", env: { GITHUB_TOKEN: "literal1" } }, /not allowlisted/);
  refuse({ id: "x", env: { RK_CONSUMER_RUN_ROOT: "toString" } }, /unknown provider/);
  refuse({ id: "x", env: {}, cwd: "/" }, /unknown fields/);
  refuse({ id: "x", args: ["--manifest-path=/etc"] }, /args invalid/);
  refuse({ id: "x", features: ["a,b"] }, /features invalid/);
  refuse({ id: "x", features: ["-Zunstable"] }, /features invalid/);
  refuse({ id: "x", features: ["--config"] }, /features invalid/);
  refuse({ id: "x", hosts: ["linux"] }, /hosts invalid/);
  assert.throws(() => parseRuns([{ id: "a" }, { id: "a" }]), /unique/);
  assert.throws(() => parseRuns([]), /non-empty/);
});
test("a missing provider prerequisite refuses; an other-OS run is only not-applicable", () => {
  const runs = parseRuns([{ id: "browser", env: { RK_R2_CFT_ROOT: "cftRoot", RK_R2_CFT_SHA256: "cftSha256", RK_R2_HEADLESS_SHELL_SHA256: "headlessShellSha256", RK_R2_SCRATCH: "freshRunRoot" } }, { id: "mac", hosts: ["darwin"], env: { RK_CONSUMER_HELPER_NAME: "signedHelper", RK_CONSUMER_HELPER_TEAM_ID: "signedHelper" } }]);
  const scratchRoot = fs.realpathSync(os.tmpdir());
  assert.throws(() => planRuns({ runs, env: {}, scratchRoot, platform: "win32" }), /prerequisite missing: cftRoot/);
  const temp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-cft-"))); const cft = path.join(temp, "cft"); fs.mkdirSync(cft);
  const good = { RUNNER_TEMP: temp, RK_R2_CFT_ROOT: cft, RK_R2_CFT_SHA256: "a".repeat(64), RK_R2_HEADLESS_SHELL_SHA256: "b".repeat(64) };
  try {
    assert.throws(() => planRuns({ runs, env: { ...good, RK_R2_CFT_SHA256: "xyz" }, scratchRoot: temp, platform: "win32" }), /prerequisite missing: cftSha256/);
    assert.throws(() => planRuns({ runs, env: { ...good, RK_R2_HEADLESS_SHELL_SHA256: undefined }, scratchRoot: temp, platform: "win32" }), /prerequisite missing: headlessShellSha256/);
    assert.throws(() => planRuns({ runs, env: { ...good, RK_R2_CFT_ROOT: path.join(os.homedir(), "elsewhere") }, scratchRoot: temp, platform: "win32" }), /prerequisite missing: cftRoot/);
    assert.throws(() => planRuns({ runs, env: good, scratchRoot: path.join(temp, "absent"), platform: "win32" }), /prerequisite missing: freshRunRoot/);
    const plan = planRuns({ runs, env: good, scratchRoot: temp, platform: "win32" });
    assert.deepEqual(plan.map(r => r.status), ["pending", "not-applicable"]); assert.equal(plan[0].values.RK_R2_HEADLESS_SHELL_SHA256, "b".repeat(64));
    // macOS never forwards the Windows-only headless-shell name, and the helper run then needs its provider.
    const mac = planRuns({ runs: runs.slice(0, 1), env: good, scratchRoot: temp, platform: "darwin" }); assert.equal(Object.hasOwn(mac[0].values, "RK_R2_HEADLESS_SHELL_SHA256"), false);
    assert.throws(() => planRuns({ runs, env: good, scratchRoot: temp, platform: "darwin" }), /prerequisite missing: signedHelper/);
    assert.equal(planRuns({ runs, env: { ...good, RK_CONSUMER_HELPER_NAME: "RightKitHelper", RK_CONSUMER_HELPER_TEAM_ID: "ABCDE12345" }, scratchRoot: temp, platform: "darwin" })[1].status, "pending");
    assert.throws(() => planRuns({ runs: parseRuns([{ id: "mac", hosts: ["darwin"] }]), env: {}, scratchRoot: temp, platform: "win32" }), /no declared run applies/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});
test("committed credentials & browser inventory declare their reviewed runs", () => {
  const cred = cargoRecipe({ root: committedRoot, unit: { name: "rightkit-credentials", version: "0.4.2" } });
  assert.deepEqual(cred.runs.map(r => [r.id, r.args, r.features, r.hosts]), [["core", ["core"], [], null], ["native-db", ["native-db"], ["sqlcipher-system"], ["win32", "darwin"]], ["native-helper", ["native-helper"], [], ["darwin"]]]);
  assert.ok(cred.evidence.dependency.features.includes("db-adapter"));
  assert.deepEqual(cred.runs[2].env, { RK_CONSUMER_HELPER_NAME: "signedHelper", RK_CONSUMER_HELPER_TEAM_ID: "signedHelper" });
  assert.equal(cred.evidence.files["prepare_signed_helper.py"], "5416b80b5b81ef50528033bbc9b91131a73c421a66764b3390331dd80fa0cd6c");
  assert.deepEqual(Object.keys(cred.evidence.files).sort(), ["Cargo.toml", "README.md", "prepare_signed_helper.py", "qualification-helper/Info.plist", "qualification-helper/README.md", "qualification-helper/entitlements.plist", "qualification-helper/main.swift", "src/main.rs"]);
  const browser = cargoRecipe({ root: committedRoot, unit: { name: "rightkit-browser", version: "0.3.5" } });
  assert.deepEqual(Object.keys(browser.runs[0].env).sort(), ["RK_R2_CFT_ROOT", "RK_R2_CFT_SHA256", "RK_R2_HEADLESS_SHELL_SHA256", "RK_R2_SCRATCH"]);
});
test("shared run executor builds Cargo args, records each status and stops at the first failure", () => {
  const plan = [{ id: "a", args: ["core"], features: [], status: "pending", values: {} }, { id: "b", args: [], features: ["f1", "f2"], status: "not-applicable", values: {} }, { id: "c", args: ["x"], features: ["f1"], status: "pending", values: { K: "v" } }, { id: "d", args: [], features: [], status: "pending", values: {} }];
  const evidence = plan.map(({ values, ...e }) => ({ ...e })); const calls = [];
  assert.throws(() => executeRuns({ plan, evidence, execute: (args, env) => { calls.push([args, env]); if (args.includes("x")) throw new Error("boom"); return "out"; } }), /boom/);
  assert.deepEqual(calls, [[["run", "--locked", "--offline", "--", "core"], {}], [["run", "--locked", "--offline", "--features", "f1", "--", "x"], { K: "v" }]]);
  assert.deepEqual(evidence.map(e => e.status), ["passed", "not-applicable", "failed", "pending"]);
  assert.deepEqual(cargoRunArgs({ features: [], args: [] }), ["run", "--locked", "--offline"]);
});
test("native helper planning applies only to credentials on macOS and otherwise refuses instead of skipping", () => {
  const row = { id: "native-helper", args: ["native-helper"], features: [], status: "pending", values: { RK_CONSUMER_HELPER_NAME: NATIVE_HELPER.name, RK_CONSUMER_HELPER_TEAM_ID: NATIVE_HELPER.teamId } };
  const credentials = { name: "rightkit-credentials" }; const source = "/Users/runner/approved/RightKitCredentialsQualification.app";
  assert.equal(planNativeHelper({ unit: { name: "rightkit-logs" }, plan: [row], env: {}, platform: "darwin" }), null);
  assert.equal(planNativeHelper({ unit: credentials, plan: [{ ...row, status: "not-applicable", values: {} }], env: {}, platform: "win32" }), null);
  assert.deepEqual(planNativeHelper({ unit: credentials, plan: [row], env: { RK_SOURCE_HELPER_APP: source }, platform: "darwin" }), { sourceApp: source });
  assert.throws(() => planNativeHelper({ unit: credentials, plan: [row], env: {}, platform: "darwin" }), /prerequisite missing: signedHelperSource/);
  assert.throws(() => planNativeHelper({ unit: credentials, plan: [{ ...row, status: "not-applicable" }], env: { RK_SOURCE_HELPER_APP: source }, platform: "darwin" }), /requires the reviewed native-helper run/);
  assert.throws(() => planNativeHelper({ unit: credentials, plan: [row, { ...row }], env: { RK_SOURCE_HELPER_APP: source }, platform: "darwin" }), /requires the reviewed native-helper run/);
  assert.throws(() => planNativeHelper({ unit: credentials, plan: [{ ...row, args: ["native-helper", "extra"] }], env: { RK_SOURCE_HELPER_APP: source }, platform: "darwin" }), /requires the reviewed native-helper run/);
  assert.deepEqual(cargoRunArgs({ features: [], args: NATIVE_HELPER.launchArgs }), NATIVE_HELPER.argv);
});
test("native helper staging requires the reviewed preparer bytes; execution routes only the pinned argv to the staged bundle", t => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-helper-stage-"))); t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const recipe = path.join(dir, "recipe"); fs.mkdirSync(recipe); fs.writeFileSync(path.join(recipe, NATIVE_HELPER.script), "reviewed preparer");
  const target = path.join(dir, "target"); fs.mkdirSync(path.join(target, "debug"), { recursive: true }); fs.writeFileSync(path.join(target, "debug", NATIVE_HELPER.consumerBinary), "default consumer");
  const calls = []; const run = (command, args, extraEnv) => { calls.push({ command, args, extraEnv }); return ""; };
  assert.throws(() => stageNativeHelper({ recipeDirectory: recipe, reviewedFiles: { [NATIVE_HELPER.script]: "0".repeat(64) }, scratch: dir, targetDir: target, sourceApp: "/x/RightKitCredentialsQualification.app", run }), /preparer differs from its reviewed hash/);
  assert.deepEqual(calls, [], "an unreviewed preparer is never executed");
  const staged = { launch: path.join(dir, "bundle", "consumer") }; const log = [];
  const execute = nativeHelperExecute({ helper: {}, staged, cargo: (args, values) => { log.push(["cargo", args]); return "cargo-out"; }, launch: (exe, args, values) => { log.push(["launch", exe, args, values]); return `${NATIVE_HELPER.success}\n`; } });
  assert.equal(execute(["run", "--locked", "--offline", "--", "core"], {}), "cargo-out");
  assert.match(execute(NATIVE_HELPER.argv, { K: "v" }), /assertions passed/);
  assert.deepEqual(log, [["cargo", ["run", "--locked", "--offline", "--", "core"]], ["launch", staged.launch, ["native-helper"], { K: "v" }]]);
  assert.throws(() => nativeHelperExecute({ helper: {}, staged: null, cargo: () => "", launch: () => "" })(NATIVE_HELPER.argv, {}), /requires a staged signed helper bundle/);
  assert.equal(nativeHelperExecute({ helper: null, staged: null, cargo: () => "plain", launch: () => assert.fail("no staging off-host") })(NATIVE_HELPER.argv, {}), "plain");
});
