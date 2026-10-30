// Local registry-consumer stage of the crate pre-flight: build the unit's committed PK example (ci/consumers/<unit>)
// against the local crate through the broker and run it the way the hosted verify does, before any dispatch.
//
// Why: browser 0.3.5 failed hosted qualification three times in a row (runs 39892614402, 39895591570, 39898450964), each
// on a defect in its consumer fixture that the local pre-flight never exercised: a compile error, a wrong executable
// pin, then a cookie read-back through the wrong scope. Building and running the fixture locally finds all three in
// one pass.
//
// What is the same as hosted (scripts/ci/registry-consumer.mjs): the committed recipe files, the reviewed run plan
// (consumer-recipes.mjs parseRuns/planRuns, per-run features, args and env providers), the pinned Chrome for Testing
// (provision-chrome.mjs + provision-r2-cft-env.mjs resolveR2CftEnv) and a fresh run root per run.
// What differs, stated in the receipt: the unit comes from the checkout through [patch.crates-io] instead of the sealed
// archive, the lock is generated rather than --locked/--offline, and the built binary is executed directly (the broker
// admits builds; executing the product of a build is not a build). Runs whose env needs a provider this host must not
// reproduce (real user credential stores, a signed nested helper) are recorded as not run locally, with the reason;
// hosted qualification still runs them.
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { cargoRecipe, parseRuns, planRuns } from "./consumer-recipes.mjs";

/** Providers a developer host may resolve for a local run; every other provider is hosted-only. */
export const LOCAL_PROVIDERS = new Set(["freshRunRoot", "cftRoot", "cftSha256", "headlessShellSha256"]);
const CHROME_PROVIDERS = new Set(["cftRoot", "cftSha256", "headlessShellSha256"]);

/** The check directory's manifest: the committed recipe manifest plus one patch pointing the unit at the checkout, and
 * one more for each sibling crate in `siblings` ({ name, rel }). A unit's path dependency is built from the checkout
 * even when the unit is patched, so a sibling the recipe also names directly (rightkit-automation -> rightkit-daemon)
 * must come from the same place or the recipe sees two copies of it (E0308, d137a9c6). */
export function patchedManifest(manifest, unit, crateDirFromCheck, siblings = []) {
  if (/^\[patch\./m.test(manifest)) throw new Error(`${unit}: a committed recipe manifest must not carry its own [patch] table`);
  const rel = crateDirFromCheck.split(path.sep).join("/");
  const extra = siblings.map((s) => `${s.name} = { path = ${JSON.stringify(s.rel.split(path.sep).join("/"))} }\n`).join("");
  return `${manifest.replace(/\s*$/, "\n")}\n# Local pre-flight only (rk-consumer-stage.mjs): the unit is built from this checkout, not the sealed archive.\n[patch.crates-io]\n${unit} = { path = ${JSON.stringify(rel)} }\n${extra}`;
}

/** RightKit crates the recipe names directly that the unit also depends on by path: they must be patched with it. */
export function pathSiblings(recipeManifest, unitManifest, unit) {
  const named = new Set([...recipeManifest.matchAll(/^(rightkit-[a-z0-9-]+)\s*=/gm)].map((m) => m[1]).filter((n) => n !== unit));
  const out = [];
  for (const m of unitManifest.matchAll(/^(rightkit-[a-z0-9-]+)\s*=\s*\{[^\n]*\bpath\s*=\s*"\.\.\/(rightkit-[a-z0-9-]+)"/gm)) {
    if (named.has(m[1]) && m[1] === m[2]) out.push(m[1]);
  }
  return [...new Set(out)].sort();
}

/** Which runs this host executes locally, and why the others are not. */
// Units whose reviewed consumer can only run where the hosted workflow prepares its fixtures. The local stage still
// builds the recipe against the checkout (attested, before the run is skipped); the run itself stays hosted evidence.
export const HOSTED_ONLY_RUNS = new Map([
  ["rightkit-fs", "needs the mounted native fixture volumes that fs-native-fixtures.mjs sets up on GitHub Actions (consumer-recipes.mjs:131, nativeFixture)"],
]);

export function localRunPlan(runs, platform = process.platform, unit = null) {
  const declared = runs ?? [{ id: "default", features: [], args: [], hosts: null, env: {} }];
  const hostedOnly = unit ? HOSTED_ONLY_RUNS.get(unit) : undefined;
  return declared.map((run) => {
    if (hostedOnly) return { run, local: false, buildOnly: true, reason: hostedOnly };
    if (run.hosts && !run.hosts.includes(platform)) return { run, local: false, reason: `not applicable on ${platform}` };
    const foreign = Object.values(run.env ?? {}).filter((p) => !LOCAL_PROVIDERS.has(p));
    if (foreign.length) return { run, local: false, reason: `needs ${[...new Set(foreign)].join(", ")}, which only the hosted runner provides` };
    return { run, local: true, needsChrome: Object.values(run.env ?? {}).some((p) => CHROME_PROVIDERS.has(p)) };
  });
}

/** The binary cargo builds for the recipe's package, under the broker-managed target directory. */
export function consumerBinary(targetDir, packageName, platform = process.platform, exists = fs.existsSync) {
  const exe = platform === "win32" ? `${packageName}.exe` : packageName;
  const candidates = [];
  if (fs.existsSync(targetDir)) {
    for (const entry of fs.readdirSync(targetDir, { withFileTypes: true })) if (entry.isDirectory() && entry.name.includes("-")) candidates.push(path.join(targetDir, entry.name, "debug", exe));
  }
  candidates.push(path.join(targetDir, "debug", exe));
  return candidates.find((c) => exists(c)) ?? null;
}

/**
 * Run the stage for one unit. `runCommand(command, args, { cwd, env, timeoutMinutes, logFile })` is the pre-flight's own
 * spawn helper; `attest(requestOutput, { nonce, argv, workspace })` verifies a brokered build row. Never throws for a
 * fixture failure: returns { ok:false, failedAt, reason } so the pre-flight records it and refuses dispatch.
 */
export async function consumerStage({ root, unit, version, sha, logDir, env, runCommand, attest, provisionChrome, platform = process.platform, spawnRun }) {
  const result = { schema: "rk-consumer-stage.v1", proof: "local-source fixture proof (not sealed-artifact qualification)", unit, version, ok: false, build: [], runs: [], differences: ["unit (and the path siblings listed in patchedSiblings) from checkout via [patch.crates-io]", "lock generated (not --locked/--offline)", "binary executed directly"] };
  let recipe;
  try { recipe = cargoRecipe({ root, unit: { name: unit, version } }); } catch (error) { return { ...result, failedAt: "recipe", reason: String(error.message ?? error) }; }
  result.pkId = recipe.evidence.pkId;
  const runs = recipe.runs ? parseRuns(recipe.runs.map(({ id, features, args, hosts, env: e }) => ({ id, features, args, ...(hosts ? { hosts } : {}), env: e }))) : null;
  const plan = localRunPlan(runs, platform, unit);
  const checks = path.join(root, ".consumer-checks");
  fs.mkdirSync(checks, { recursive: true });
  const checkDir = fs.mkdtempSync(path.join(checks, `${unit}-${sha.slice(0, 12)}-`));
  result.checkDir = path.relative(root, checkDir);
  fs.cpSync(recipe.directory, checkDir, { recursive: true });
  for (const lock of ["Cargo.lock"]) if (fs.existsSync(path.join(checkDir, lock))) fs.renameSync(path.join(checkDir, lock), path.join(checkDir, `${lock}.committed`));
  const recipeManifest = fs.readFileSync(path.join(checkDir, "Cargo.toml"), "utf8");
  let unitManifest = "";
  try { unitManifest = fs.readFileSync(path.join(root, "crates", unit, "Cargo.toml"), "utf8"); } catch { /* no sibling patches */ }
  // A sibling is patched only when the checkout has the exact version the recipe pins; otherwise Cargo would ignore the
  // patch, and the receipt must not claim it.
  const siblings = pathSiblings(recipeManifest, unitManifest, unit)
    .filter((name) => {
      const pin = new RegExp(`^${name}\\s*=\\s*\\{[^\\n]*version\\s*=\\s*"=([^"]+)"`, "m").exec(recipeManifest)?.[1];
      let local = null;
      try { local = /^version\s*=\s*"([^"]+)"/m.exec(fs.readFileSync(path.join(root, "crates", name, "Cargo.toml"), "utf8"))?.[1] ?? null; } catch { /* absent */ }
      return pin !== undefined && local === pin;
    })
    .map((name) => ({ name, rel: path.relative(checkDir, path.join(root, "crates", name)) }));
  result.patchedSiblings = siblings.map((s) => s.name);
  fs.writeFileSync(path.join(checkDir, "Cargo.toml"), patchedManifest(recipeManifest, unit, path.relative(checkDir, path.join(root, "crates", unit)), siblings));
  const packageName = recipe.manifest.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
  if (!packageName) return { ...result, failedAt: "recipe", reason: "recipe manifest has no package name" };

  // Chrome for Testing, pinned and verified, exactly as the hosted verify provisions it; only when a local run needs it.
  let chromeEnv = {};
  const scratchRoot = path.join(logDir, "consumer-tmp");
  fs.mkdirSync(scratchRoot, { recursive: true });
  if (plan.some((p) => p.local && p.needsChrome)) {
    try { chromeEnv = await provisionChrome({ runnerTemp: scratchRoot }); } catch (error) { return { ...result, failedAt: "chrome-provision", reason: String(error.message ?? error) }; }
  }

  const built = new Map();
  for (const { run, local, buildOnly, reason } of plan) {
    if (!local && !buildOnly) { result.runs.push({ id: run.id, status: "not-run-locally", reason }); continue; }
    const features = run.features ?? [];
    const key = features.join(",");
    if (!built.has(key)) {
      const argv = ["build", ...(features.length ? ["--features", key] : [])];
      const nonce = crypto.randomBytes(32).toString("hex");
      const logFile = path.join(logDir, `registry-consumer-build${key ? `-${key.replace(/[^a-z0-9]+/gi, "_")}` : ""}.log`);
      const r = await runCommand("rustright", ["cargo", ...argv], { cwd: checkDir, env: { ...env, RUSTRIGHT_ATTEST_NONCE: nonce }, timeoutMinutes: 60, logFile });
      const attestation = attest(r.out, { nonce, argv, workspace: checkDir });
      const entry = { argv: `cargo ${argv.join(" ")}`, argvList: argv, exit: r.code, timedOut: r.timedOut, seconds: r.seconds, attestation, log: logFile, ok: r.code === 0 && !r.timedOut && attestation.verified === true };
      result.build.push(entry);
      if (!entry.ok) return { ...result, failedAt: "build", reason: attestation.verified === true ? `consumer build failed (exit ${r.code})` : `consumer build not attested: ${attestation.reason}`, lines: r.out.split("\n").filter((l) => /^error(\[|:)/.test(l)).slice(0, 12) };
      const meta = await runCommand("rustright", ["cargo", "metadata", "--format-version", "1", "--no-deps"], { cwd: checkDir, env, timeoutMinutes: 5, logFile: path.join(logDir, "registry-consumer-metadata.log") });
      let targetDir = null;
      try { targetDir = JSON.parse(meta.out.slice(meta.out.indexOf("{"))).target_directory; } catch { targetDir = null; }
      const binary = targetDir && consumerBinary(targetDir, packageName, platform);
      if (!binary) return { ...result, failedAt: "build", reason: `built binary ${packageName} not found under the broker target directory` };
      built.set(key, binary);
    }
    // A hosted-only unit still proves it compiles against the checkout (the attested build above); only the run is hosted.
    if (!local) { result.runs.push({ id: run.id, status: "not-run-locally", reason }); continue; }
    let values;
    try {
      [{ values }] = planRuns({ runs: [run], env: { ...env, ...chromeEnv, RUNNER_TEMP: scratchRoot }, scratchRoot, platform });
    } catch (error) { return { ...result, failedAt: `run ${run.id}`, reason: String(error.message ?? error) }; }
    const started = Date.now();
    const logFile = path.join(logDir, `registry-consumer-run-${run.id}.log`);
    let r; let via; let attestation = null; let runArgv = null;
    if (Object.keys(values).length === 0) {
      // No run environment: the run itself goes through the broker (`cargo run`, attested like a build row).
      via = "broker";
      const argv = ["run", ...(features.length ? ["--features", key] : []), ...((run.args ?? []).length ? ["--", ...run.args] : [])];
      const nonce = crypto.randomBytes(32).toString("hex");
      r = await runCommand("rustright", ["cargo", ...argv], { cwd: checkDir, env: { ...env, RUSTRIGHT_ATTEST_NONCE: nonce }, timeoutMinutes: 30, logFile });
      attestation = attest(r.out, { nonce, argv, workspace: checkDir });
      runArgv = argv;
      if (attestation.verified !== true) r = { ...r, code: r.code === 0 ? 1 : r.code, out: `${r.out}\nconsumer run not attested: ${attestation.reason}` };
    } else {
      // The broker forwards a client's environment only to `cargo test` (rustright docs/using-the-broker.md, forwardTestEnv),
      // and drops it for `cargo run` (observed 2026-10-23: the browser fixture exits NotPresent without RK_R2_*). A run that
      // needs its reviewed environment executes the broker-built binary directly; the build is the attested part.
      via = "direct (run env is not forwarded by the broker for cargo run)";
      r = (spawnRun ?? defaultSpawnRun)(built.get(key), run.args ?? [], { cwd: checkDir, env: { ...env, ...values, RUNNER_TEMP: scratchRoot }, logFile });
    }
    const entry = { id: run.id, via, argvList: runArgv, attestation, status: r.code === 0 && !r.timedOut ? "passed" : "failed", exit: r.code, timedOut: r.timedOut, seconds: Math.round((Date.now() - started) / 1000), envNames: Object.keys(values).sort(), log: logFile, tail: r.out.split("\n").filter(Boolean).slice(-8) };
    result.runs.push(entry);
    if (entry.status !== "passed") return { ...result, failedAt: `run ${run.id}`, reason: `consumer run ${run.id} exited ${r.code}${r.timedOut ? " (timed out)" : ""}`, lines: entry.tail };
  }
  result.ok = true;
  return result;
}

function defaultSpawnRun(binary, args, { cwd, env, logFile }) {
  const r = spawnSync(binary, args, { cwd, env, encoding: "utf8", timeout: 10 * 60_000, maxBuffer: 64 * 1024 * 1024 });
  const out = `${r.stdout ?? ""}${r.stderr ?? ""}${r.error ? `\n${r.error.message}` : ""}`;
  fs.writeFileSync(logFile, out);
  return { code: r.status ?? 1, out, timedOut: r.error?.code === "ETIMEDOUT" };
}
