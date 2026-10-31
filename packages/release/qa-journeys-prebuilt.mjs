// GB-79 (GATE test stage E1/E6/E11): `rightkit-qa journeys run` on a snapshot stage, as the gate's own build-once route (capability `journeys-prebuilt-input`).
//
// `journeys run` spawns its own `rustright cargo ... --no-run`, which a gate-carried stage cannot allow: a snapshot stage compiles once, in the carrier, and QA runs
// an already-built, already-signed product outside the broker. So the gate takes the build out of QA's hands:
//   1. ask QA what it would build, without building (`journeys run ... --print-build-plan`: the declared build command and each artifact role's marker policy);
//   2. carry that build (`cargo test ... --no-run --message-format=json`) through the carrier: one signed, ledgered broker request, its receipt verified;
//   3. have QA name the executable each role resolves to in the carried listing (`--print-build-plan --from-cargo-json <listing>`), and check each lies inside the
//      receipt's verified output directory;
//   4. sign every product (a role whose marker is required) with the product's one identity (suite-signing.mjs runSignStep), outside the broker, from the receipt's
//      output directory; a product that cannot be signed refuses and nothing launches;
//   5. write the attestation (`rightkit.journeys.prebuilt-input/1`: the broker request id and digest, the listing's sha256, the carried command, the sha256 of
//      every executable QA will launch) and run QA directly with `--from-cargo-json <listing> --prebuilt-attestation <file>`, the signing evidence in its environment.
// QA verifies all of it again (crates/rightkit-qa/src/journeys/prebuilt.rs build_from_prebuilt) and never compiles. An empty journey set is never green: the plan
// step fails when nothing is selected, QA's own verdict needs executed journeys, and an empty listing is refused on both sides.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { runCarriedCargo } from "./carried-cargo.mjs";
import { isInside, realish } from "./qa-snapshot.mjs";
import { runSignStep } from "./suite-signing.mjs";
import { QA_APP_BINARY_ENV, QA_SIGNED_FILES_ENV, QA_SIGNING_EVIDENCE_ENV } from "./suite-signing-policy.mjs";

export const JOURNEYS_PREBUILT_CAPABILITY = "journeys-prebuilt-input";
export const PREBUILT_SCHEMA = "rightkit.journeys.prebuilt-input/1";
export const BUILD_PLAN_SCHEMA = "rightkit.journeys.build-plan/1";
/** The suite name the sign step reads the carried receipt's output directory under. */
export const JOURNEYS_BUILD_SUITE = "journeys-build";
/** The exact key set of the attestation (QA reads it with unknown fields denied). */
export const ATTESTATION_KEYS = Object.freeze(["schema", "request_id", "request_sha256", "listing_sha256", "command", "outputs"]);

const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const words = (args) => args.map(String);
const OWN_FLAGS = ["--from-cargo-json", "--prebuilt-attestation", "--print-build-plan"];
const flagOf = (w) => { const at = w.indexOf("="); return at > 0 && w.startsWith("--") ? w.slice(0, at) : w; };

/** True when this QA argv is `journeys run ...`. */
export const isJourneysRun = (args) => { const w = words(args); return w[0] === "journeys" && w[1] === "run"; };

/** Why a `journeys run` argv cannot be carried, or null: the listing and attestation are the gate's to write, never the step's (an input the gate did not verify). */
export function journeysRunRefusal(args) {
  const own = words(args).filter((w) => w.startsWith("-")).map(flagOf);
  const named = OWN_FLAGS.find((flag) => own.includes(flag));
  return named ? `${named} is the gate's own: a journeys step names no listing, attestation or plan flag (the gate carries the build and hands QA the verified input)` : null;
}

/**
 * Whether the installed `rightkit-qa` implements the prebuilt journey input: its `journeys run --help` names the capability. A binary that cannot be asked, or does
 * not name it, has no such mode.
 */
export function qaPrebuiltCapability(qa, { spawn = spawnSync, env = process.env } = {}) {
  try {
    const r = spawn(qa, ["journeys", "run", "--help"], { encoding: "utf8", env, windowsHide: true, timeout: 20000 });
    return r?.status === 0 && String(r.stdout ?? "").includes(JOURNEYS_PREBUILT_CAPABILITY);
  } catch { return false; }
}

/** The refusal text of a planned journeys step on a snapshot stage whose QA lacks the capability, naming it. */
export const journeysCapabilityRefusal = (name) => `${name}: \`rightkit-qa journeys run\` needs capability "${JOURNEYS_PREBUILT_CAPABILITY}" and the installed rightkit-qa does not advertise it (\`rightkit-qa journeys run --help\`): without it QA would compile through the broker itself, and a gate-carried stage must build once, sign, then run prebuilt bytes. Nothing ran`;

const fail = (code, message, status = 101) => Object.assign(new Error(`${code}: ${message}`), { code, status });

function parsePlan(result, what) {
  if (result?.error) throw fail("RK_QA_JOURNEYS_PLAN", `${what}: rightkit-qa did not start (${result.error.message})`);
  if (result?.status !== 0) throw fail("RK_QA_JOURNEYS_PLAN", `${what}: rightkit-qa exited ${result?.status}: ${String(result?.stderr ?? "").trim().slice(0, 600) || "no message"}; nothing was built`);
  let plan;
  try { plan = JSON.parse(String(result.stdout).trim().split(/\r?\n/).filter(Boolean).pop() ?? ""); } catch { throw fail("RK_QA_JOURNEYS_PLAN", `${what}: rightkit-qa printed no build plan`); }
  if (plan?.schema !== BUILD_PLAN_SCHEMA || !Array.isArray(plan.command) || !Array.isArray(plan.artifacts) || typeof plan.cwd !== "string") throw fail("RK_QA_JOURNEYS_PLAN", `${what}: the build plan is not a ${BUILD_PLAN_SCHEMA} document`);
  return plan;
}

/**
 * The gate's build-once run of one `journeys run` step. `args` is QA's argv (`journeys run ...`); `map` is the carrier map. Resolves `{ status }` (QA's own exit status)
 * or throws an Error with a typed `code` (nothing ran, or QA was never started). Every effect is injectable: `spawn` (QA), `carry` (the carrier), `signStep`, `loadConfig`.
 */
export function runJourneysPrebuilt({ qa, args, cwd = process.cwd(), env = process.env, map, platform = process.platform, spawn = spawnSync, carry = runCarriedCargo, signStep = runSignStep, signDeps = {}, loadConfig = defaultConfig, log = (m) => process.stderr.write(`${m}\n`) }) {
  const given = words(args);
  if (!isJourneysRun(given)) throw fail("RK_QA_JOURNEYS_USAGE", `rightkit-qa ${given.join(" ")} is not a journeys run step`);
  const own = journeysRunRefusal(given);
  if (own) throw fail("RK_QA_JOURNEYS_FLAG", own);
  const section = map.broker ?? {};
  const snapshot = section.snapshot?.path;
  const checkouts = [section.checkout, env.RK_GATE_CHECKOUT].filter((c) => typeof c === "string" && c);
  const run = (extra, what) => spawn(qa, [...given, ...extra], { cwd, env, encoding: "utf8", windowsHide: true, maxBuffer: 1 << 26 });
  // 1. what QA would build (nothing is created or built).
  const plan = parsePlan(run(["--print-build-plan"]), "the build plan");
  if (plan.command[0] !== "rustright" || plan.command[1] !== "cargo" || plan.command.length < 3) throw fail("RK_QA_JOURNEYS_PLAN", `the declared build ${JSON.stringify(plan.command)} is not a \`rustright cargo ...\` command; a snapshot stage carries Cargo builds only`);
  if (plan.artifacts.length === 0) throw fail("RK_QA_JOURNEYS_EMPTY", "the journeys build declares no artifact: an empty journey set is never green");
  if (!snapshot || !isInside(plan.cwd, snapshot) || checkouts.some((c) => isInside(plan.cwd, c))) throw fail("RK_QA_JOURNEYS_PLACE", `the journeys build directory ${plan.cwd} is not inside the commit's snapshot${snapshot ? ` (${snapshot})` : ""}`);
  // 2. the carried build.
  const dir = path.join(path.dirname(map.ledgerDir), "qa-listings");
  if (isInside(dir, snapshot) || checkouts.some((c) => isInside(dir, c))) throw fail("RK_QA_JOURNEYS_PLACE", `the listing directory ${dir} is inside the snapshot or the primary checkout`);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const stem = path.join(realish(dir), `${process.pid}-${Date.now().toString(36)}-${crypto.randomBytes(4).toString("hex")}`);
  const listingFile = `${stem}.jsonl`; const attestationFile = `${stem}.attestation.json`;
  const cargoWords = plan.command.slice(2);
  const built = carry(cargoWords, { cwd: plan.cwd, env, stdio: "capture" }, { map });
  if (built.stderr) process.stderr.write(built.stderr);
  if (built.error) throw fail("RK_QA_JOURNEYS_BUILD", built.error.message);
  if (built.status !== 0 || built.refusal || built.outputDir === undefined || !built.requestId || !/^[0-9a-f]{64}$/.test(built.requestDigest ?? "")) {
    throw Object.assign(fail("RK_QA_JOURNEYS_BUILD", `the carried build \`cargo ${cargoWords.join(" ")}\` did not complete with a verified receipt (exit ${built.status}${built.refusal ? `, ${built.refusal.code}` : ""}); QA did not run`), { status: built.status || 101 });
  }
  let signedDir = null;
  try {
    const listing = String(built.stdout ?? "");
    fs.writeFileSync(listingFile, listing, { mode: 0o600 });
    // 3. the executable of each role, as QA resolves it from this listing; each must be a file the receipt's output directory holds.
    const resolved = parsePlan(run(["--print-build-plan", "--from-cargo-json", listingFile]), "the role to executable mapping");
    const roles = resolved.artifacts;
    const outputs = [];
    for (const role of roles) {
      if (typeof role.executable !== "string" || !role.executable) throw fail("RK_QA_JOURNEYS_PLAN", `role ${role.role} resolves to no executable in the carried listing`);
      const real = realish(role.executable);
      if (!isInside(real, built.outputDir)) throw fail("RK_QA_JOURNEYS_PLACE", `role ${role.role}: ${role.executable} is not inside the receipt's output directory ${built.outputDir}`);
      let data;
      try { if (!fs.statSync(real).isFile()) throw new Error("not a file"); data = fs.readFileSync(real); } catch (e) { throw fail("RK_QA_JOURNEYS_PLACE", `role ${role.role}: ${role.executable} cannot be read (${e.message})`); }
      outputs.push({ role: role.role, marker: role.marker, path: role.executable, real, sha256: sha256(data) });
    }
    if (outputs.length === 0) throw fail("RK_QA_JOURNEYS_EMPTY", "the carried build resolved no executable: an empty journey set is never green");
    // 4. sign every product before anything launches.
    let signEnv = {};
    const products = outputs.filter((o) => o.marker === "required");
    if (products.length) {
      let signed;
      try {
        const files = products.map((o) => ({ from: JOURNEYS_BUILD_SUITE, path: path.relative(realish(built.outputDir), o.real).split(path.sep).join("/") }));
        signed = signStep({ name: "journeys-sign", sign: { files }, appRoot: cwd, config: loadConfig(cwd), env, platform, outputDirsOf: (suite) => (suite === JOURNEYS_BUILD_SUITE ? [built.outputDir] : null), deps: signDeps, log });
      } catch (e) { throw fail(e.code ?? "RK_QA_JOURNEYS_UNSIGNED", `a product of the journeys build could not be signed: ${e.message}; an unsigned product is never launched, and QA did not run`); }
      signedDir = signed.signedDir;
      signEnv = { [QA_SIGNING_EVIDENCE_ENV]: signed.evidenceFile, [QA_SIGNED_FILES_ENV]: JSON.stringify(signed.signedFiles ?? {}), ...(signed.appBinary ? { [QA_APP_BINARY_ENV]: signed.appBinary } : {}) };
    }
    // 5. the attestation, then QA directly against the verified input.
    const attestation = { schema: PREBUILT_SCHEMA, request_id: built.requestId, request_sha256: built.requestDigest, listing_sha256: sha256(Buffer.from(listing)), command: [...plan.command.slice(1)], outputs: outputs.map((o) => ({ path: o.path, sha256: o.sha256 })) };
    fs.writeFileSync(attestationFile, JSON.stringify(attestation), { mode: 0o600 });
    const ran = spawn(qa, [...given, "--from-cargo-json", listingFile, "--prebuilt-attestation", attestationFile], { cwd, env: { ...env, ...signEnv }, stdio: "inherit", windowsHide: true });
    if (ran.error) throw fail("RK_QA_JOURNEYS_RUN", ran.error.message);
    return { status: ran.status ?? 1, attestation };
  } finally {
    for (const f of [listingFile, attestationFile]) { try { fs.rmSync(f, { force: true }); } catch { /* pruned with the managed directory */ } }
    if (signedDir) { try { fs.rmSync(signedDir, { recursive: true, force: true }); } catch { /* the managed root is pruned */ } }
  }
}

/** The app's right-release config JSON beside the step's directory (the sign step derives the product identity from it); null when there is none. */
function defaultConfig(cwd) {
  try { return JSON.parse(fs.readFileSync(path.join(cwd, "right-release.config.json"), "utf8")); } catch { return null; }
}
