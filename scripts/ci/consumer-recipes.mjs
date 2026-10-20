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

// ---- recipe runs: reviewed inventory declares WHAT runs; this table alone decides env VALUES ----
// Values never come from the candidate or the inventory. Each provider owns a fixed set of env names, reads
// only trusted harness-side inputs (workflow env set before the candidate runs, RUNNER_TEMP) and verifies
// them. No secret or token is ever a provider input. A missing prerequisite refuses; it never skips.
const REAL = p => { try { return fs.realpathSync(p); } catch { return null; } };
const prerequisite = (provider, why) => need(false, `prerequisite missing: ${provider} (${why})`);
const hexEnv = (provider, name) => ({ env }) => { const value = env[name]; if (!/^[0-9a-f]{64}$/.test(value ?? "")) prerequisite(provider, `${name} must be a 64-hex SHA-256 provisioned by the workflow`); return { [name]: value }; };
export const RUN_ENV_PROVIDERS = Object.freeze({
  freshRunRoot: { names: ["RK_CONSUMER_RUN_ROOT", "RK_R2_SCRATCH"], resolve: ({ scratchRoot, names }) => {
    need(scratchRoot && fs.existsSync(scratchRoot), "prerequisite missing: freshRunRoot (RUNNER_TEMP scratch root absent)");
    const dir = fs.mkdtempSync(path.join(scratchRoot, "rk-run-root-")); return Object.fromEntries(names.map(n => [n, dir]));
  } },
  literal1: { names: ["RIGHTKIT_TEST_REAL_USER_STORES"], resolve: ({ names }) => Object.fromEntries(names.map(n => [n, "1"])) },
  cftRoot: { names: ["RK_R2_CFT_ROOT"], resolve: ({ env }) => {
    const raw = env.RK_R2_CFT_ROOT; const temp = env.RUNNER_TEMP && REAL(env.RUNNER_TEMP);
    if (!raw || !path.isAbsolute(raw) || !temp) prerequisite("cftRoot", "workflow must provision RK_R2_CFT_ROOT (absolute) under RUNNER_TEMP");
    const real = REAL(raw);
    if (!real || fs.lstatSync(raw).isSymbolicLink() || !fs.statSync(real).isDirectory() || !within(temp, real)) prerequisite("cftRoot", "RK_R2_CFT_ROOT must be an existing real directory inside RUNNER_TEMP");
    return { RK_R2_CFT_ROOT: real };
  } },
  cftSha256: { names: ["RK_R2_CFT_SHA256"], resolve: hexEnv("cftSha256", "RK_R2_CFT_SHA256") },
  // Only the Windows managed-CFT binding reads this name; elsewhere it is intentionally not set.
  headlessShellSha256: { names: ["RK_R2_HEADLESS_SHELL_SHA256"], platforms: ["win32"], resolve: hexEnv("headlessShellSha256", "RK_R2_HEADLESS_SHELL_SHA256") },
  signedHelper: { names: ["RK_CONSUMER_HELPER_NAME", "RK_CONSUMER_HELPER_TEAM_ID"], platforms: ["darwin"], resolve: ({ env }) => {
    const name = env.RK_CONSUMER_HELPER_NAME; const team = env.RK_CONSUMER_HELPER_TEAM_ID;
    if (!/^[A-Za-z0-9._-]{1,64}$/.test(name ?? "") || !/^[A-Z0-9]{10}$/.test(team ?? "")) prerequisite("signedHelper", "workflow must provision the signed nested helper name & team id");
    return { RK_CONSUMER_HELPER_NAME: name, RK_CONSUMER_HELPER_TEAM_ID: team };
  } },
});
const RUN_HOSTS = ["win32", "darwin"];
export function parseRuns(declared) {
  if (declared === undefined) return null;
  need(Array.isArray(declared) && declared.length > 0, "runs must be a non-empty array");
  const ids = new Set();
  return declared.map(run => {
    need(run && typeof run === "object" && !Array.isArray(run) && Object.keys(run).every(k => ["id", "features", "args", "hosts", "env"].includes(k)), "run declares unknown fields");
    need(typeof run.id === "string" && /^[a-z][a-z0-9-]{0,39}$/.test(run.id) && !ids.has(run.id), "run id must be unique lowercase slug"); ids.add(run.id);
    const list = (key, pattern) => { const v = run[key] ?? []; need(Array.isArray(v) && v.every(x => typeof x === "string" && pattern.test(x)), `run ${run.id} ${key} invalid`); return [...v]; };
    const features = list("features", /^[A-Za-z0-9_][A-Za-z0-9_-]*$/); const args = list("args", /^[A-Za-z0-9][A-Za-z0-9_.-]*$/);
    const hosts = run.hosts === undefined ? null : list("hosts", /^(?:win32|darwin)$/); need(hosts === null || (hosts.length > 0 && hosts.every(h => RUN_HOSTS.includes(h))), `run ${run.id} hosts invalid`);
    const env = run.env ?? {}; need(env && typeof env === "object" && !Array.isArray(env), `run ${run.id} env must map NAME to provider id`);
    for (const [name, provider] of Object.entries(env)) {
      need(typeof provider === "string" && Object.hasOwn(RUN_ENV_PROVIDERS, provider), `run ${run.id} env ${name}: unknown provider (raw values are never accepted)`);
      need(RUN_ENV_PROVIDERS[provider].names.includes(name), `run ${run.id} env name ${name} is not allowlisted for provider ${provider}`);
    }
    return { id: run.id, features, args, hosts, env: { ...env } };
  });
}

/** Resolve every run on this host before any Cargo work. Other-OS runs are not-applicable; an absent prerequisite refuses. */
export function planRuns({ runs, env, scratchRoot, platform = process.platform }) {
  const plan = (runs ?? [{ id: "default", features: [], args: [], hosts: null, env: {}, implicit: true }]).map(run => {
    const base = { id: run.id, args: run.args, features: run.features, envNames: [], status: "pending" };
    if (run.hosts && !run.hosts.includes(platform)) return { ...base, status: "not-applicable", hosts: run.hosts, values: {} };
    const values = {}; const resolved = new Map();
    for (const [name, provider] of Object.entries(run.env)) {
      const spec = RUN_ENV_PROVIDERS[provider];
      if (spec.platforms && !spec.platforms.includes(platform)) continue;
      if (!resolved.has(provider)) resolved.set(provider, spec.resolve({ env, scratchRoot, platform, names: spec.names.filter(n => Object.hasOwn(run.env, n) && run.env[n] === provider) }));
      values[name] = resolved.get(provider)[name];
    }
    return { ...base, envNames: Object.keys(values).sort(), values, implicit: Boolean(run.implicit) };
  });
  need(plan.some(r => r.status !== "not-applicable"), "no declared run applies to this host");
  return plan;
}

export const cargoRunArgs = entry => ["run", "--locked", "--offline", ...(entry.features.length ? ["--features", entry.features.join(",")] : []), ...(entry.args.length ? ["--", ...entry.args] : [])];
/** Execute a plan from planRuns through `execute(cargoArgs, env) => stdout`; fills `evidence[i].status`. Shared by R2a and R2b. */
export function executeRuns({ plan, execute, evidence }) {
  const outputs = [];
  for (const [index, entry] of plan.entries()) {
    if (entry.status === "not-applicable") { evidence[index].status = "not-applicable"; continue; }
    evidence[index].status = "running";
    try { outputs.push(String(execute(cargoRunArgs(entry), entry.values))); } catch (error) { evidence[index].status = "failed"; throw error; }
    evidence[index].status = "passed";
  }
  return outputs;
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
  const runs = parseRuns(reviewed.runs);
  const sortedFiles = Object.fromEntries(Object.entries(files).sort(([a], [b]) => a.localeCompare(b)));
  const reviewedFiles = Object.fromEntries(Object.entries(reviewed.files ?? {}).sort(([a], [b]) => a.localeCompare(b)));
  need(JSON.stringify(sortedFiles) === JSON.stringify(reviewedFiles), `recipe differs from reviewed ${reviewed.pkId} inventory; review exact full file hashes before qualification`);
  const dependency = manifest.split(/\r?\n/).find(line => line.startsWith(`${unit.name} `));
  need(dependency, "PK dependency must use reviewed single-line exact-version declaration");
  const selectedFeatures = dependency.match(/\bfeatures\s*=\s*\[([^\]]*)\]/)?.[1] ?? "";
  const features = [...selectedFeatures.matchAll(/"([A-Za-z0-9_+/-]+)"/g)].map(match => match[1]);
  return { directory, manifest, dependency, runs, evidence: { id, pkId: reviewed.pkId, sourceCommit: null, files: sortedFiles, sha256: hash(JSON.stringify(sortedFiles)), manifestSha256: files["Cargo.toml"], exampleSha256: files["src/main.rs"], dependency: { name: unit.name, version: `=${unit.version}`, defaultFeatures: !/default-features\s*=\s*false/.test(dependency), features }, runs: runs ?? null } };
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

// ---- signed nested helper (rightkit-credentials native-helper row, macOS only) ----
// The reviewed preparer copies the default-feature consumer binary & the harness-supplied, already signed helper
// into a fresh bundle; this table pins every name, argv & output so a changed recipe row refuses instead of
// silently falling back to a bare Cargo run that cannot resolve the bundled helper. No signing, no download.
export const NATIVE_HELPER = Object.freeze({
  unit: "rightkit-credentials", platform: "darwin", runId: "native-helper", name: "RightKitCredentialsQualification", teamId: "6KLGD3LLKF",
  bundleIdentifier: "org.rightkit.qualification.credentials", accessGroup: "6KLGD3LLKF.org.rightkit.qualification.credentials",
  argv: Object.freeze(["run", "--locked", "--offline", "--", "native-helper"]), launchArgs: Object.freeze(["native-helper"]),
  script: "prepare_signed_helper.py", consumerBinary: "rightkit-credentials-r2-consumer", bundleName: "CredentialsConsumer.app",
  receiptName: "credentials-qualification-helper-input.json", launch: "Contents/MacOS/consumer", inputKind: "credentials-native-helper-input",
  success: "native bundled helper consumer assertions passed",
});
const sameJson = (a, b) => JSON.stringify(a) === JSON.stringify(b);
/** Before any Cargo/fetch work: null for other units/hosts, else the validated helper input. A missing input refuses; it never skips. */
export function planNativeHelper({ unit, plan, env, platform = process.platform }) {
  if (unit.name !== NATIVE_HELPER.unit || platform !== NATIVE_HELPER.platform) return null;
  const rows = plan.filter(row => row.id === NATIVE_HELPER.runId);
  need(rows.length === 1 && rows[0].status !== "not-applicable" && sameJson(cargoRunArgs(rows[0]), NATIVE_HELPER.argv), "rightkit-credentials requires the reviewed native-helper run with exact argv on macOS");
  need(rows[0].values.RK_CONSUMER_HELPER_NAME === NATIVE_HELPER.name && rows[0].values.RK_CONSUMER_HELPER_TEAM_ID === NATIVE_HELPER.teamId, "native-helper run must carry the pinned signed helper name & team id");
  const source = env.RK_SOURCE_HELPER_APP;
  if (typeof source !== "string" || !path.isAbsolute(source) || source.split(/[\\/]/).includes("..") || path.basename(source) !== `${NATIVE_HELPER.name}.app`) prerequisite("signedHelperSource", `workflow must provision RK_SOURCE_HELPER_APP as an absolute path to ${NATIVE_HELPER.name}.app`);
  return { sourceApp: source };
}
/** Stage the unchanged signed helper + default consumer binary through the reviewed preparer; `run(command, args, extraEnv)` is the runner's own wrapper. */
export function stageNativeHelper({ recipeDirectory, reviewedFiles, scratch, targetDir, sourceApp, run }) {
  const script = path.join(recipeDirectory, NATIVE_HELPER.script);
  need(hash(fs.readFileSync(script)) === reviewedFiles?.[NATIVE_HELPER.script], "native helper preparer differs from its reviewed hash");
  const real = fs.realpathSync(scratch);
  const consumer = fs.realpathSync(path.join(targetDir, "debug", NATIVE_HELPER.consumerBinary));
  const bundle = path.join(real, NATIVE_HELPER.bundleName); const receipt = path.join(real, NATIVE_HELPER.receiptName);
  const consumerSha256 = hash(fs.readFileSync(consumer));
  run("python3", [script, "--scratch-root", real, "--consumer-executable", consumer, "--bundle", bundle, "--receipt", receipt], { RK_SOURCE_HELPER_APP: sourceApp });
  const bytes = fs.readFileSync(receipt); const input = JSON.parse(bytes); const launch = path.join(bundle, NATIVE_HELPER.launch);
  need(input.schema === 1 && input.kind === NATIVE_HELPER.inputKind && input.name === NATIVE_HELPER.name && input.teamId === NATIVE_HELPER.teamId && input.bundleIdentifier === NATIVE_HELPER.bundleIdentifier && input.accessGroup === NATIVE_HELPER.accessGroup && input.signatureVerified === true && input.nativeConsumerVerified === false, "native helper input receipt is not the reviewed signed-helper identity");
  need(input.consumerSha256 === consumerSha256 && /^[0-9a-f]{64}$/.test(input.helperTree?.sha256 ?? "") && fs.statSync(launch).isFile() && hash(fs.readFileSync(launch)) === consumerSha256, "native helper input receipt does not bind the staged default consumer & helper tree");
  return { bundle, receipt, launch, input, inputSha256: hash(bytes) };
}
/** Wrap a runner's Cargo executor: the planned native-helper row launches the staged bundle (never Cargo) & must print its assertion line. */
export const nativeHelperExecute = ({ helper, staged, cargo, launch }) => (cargoArgs, values) => {
  if (!helper || !sameJson(cargoArgs, NATIVE_HELPER.argv)) return cargo(cargoArgs, values);
  need(staged, "native-helper row requires a staged signed helper bundle");
  const output = String(launch(staged.launch, [...NATIVE_HELPER.launchArgs], values));
  need(output.includes(NATIVE_HELPER.success), "staged native-helper launch did not report its consumer assertions");
  return output;
};
