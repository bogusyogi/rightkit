// `workflows` manifest block: named workflows rendered by code, not by a lane template.
//
//   kind "custom"          one build job: triggers, steps, toolchain pins, named artifact (#465)
//   kind "swift-host"      `swift test` on Windows and macOS hosts for chosen packages (#437)
//   kind "dispatch-matrix" workflow_dispatch inputs -> plan job -> per-item matrix jobs -> collect job (#437)
//
// Every action is pinned to a 40-hex commit SHA, the workflow runs with `contents: read` only, no step may read
// the secrets context, and every checkout sets `persist-credentials: false`. Manifest values reach YAML only
// through strict allowlists, so no value can carry `${{ }}` except the fixed artifact-name tokens below.
// Each rendered file starts with the managed marker, so sync, drift and uninstall treat it like a lane file.

import { validateCheckout, applyCheckoutOptions } from "./checkout.mjs";
import {
  LANE_FILENAMES,
  RUST_HYBRID_ONLY_LANES,
  UPLOAD_ARTIFACT_PIN,
  DOWNLOAD_ARTIFACT_SHA,
  deriveProjectName,
  ownershipStep,
  parsePackageManager,
} from "./manifest.mjs";

export const WORKFLOW_KINDS = Object.freeze(["custom", "swift-host", "dispatch-matrix"]);

const CHECKOUT_PIN = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1";
const SETUP_NODE_PIN = "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0";
const RUST_TOOLCHAIN_PIN = "dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0";

const NAME_RE = /^[a-z][a-z0-9-]{0,40}$/;
const FILE_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,60}\.yml$/;
const RUNNER_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,40}$/;
const BRANCH_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const TRIGGER_PATH_RE = /^!?(?!.*\.\.)(?!\/)[A-Za-z0-9._/*-]+$/;
const GROUP_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,60}$/;
const WORK_DIR_RE = /^(?:\.|(?!.*\.\.)(?!.*\s)(?!\/)[A-Za-z0-9][A-Za-z0-9._/-]*)$/;
const STEP_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9 ._:()/+-]{0,80}$/;
const ARTIFACT_PATH_RE = /^(?!.*\.\.)(?!\/)[A-Za-z0-9._/*?-]+$/;
const ARTIFACT_TOKEN_RE = /\{(sha|run_id|run_attempt|os|matrix\.[a-z_][a-z0-9_]*)\}/g;
const ARTIFACT_LITERAL_RE = /^[A-Za-z0-9._-]+$/;
const INPUT_NAME_RE = /^[a-z][a-z0-9_]{0,39}$/;
const INPUT_TEXT_RE = /^[A-Za-z0-9 ._,:()/+-]{0,120}$/;
const INPUT_VALUE_RE = /^[A-Za-z0-9._,:/+-][A-Za-z0-9 ._,:/+-]{0,199}$|^$/;
const SWIFT_VERSION_RE = /^\d+\.\d+(?:\.\d+)?$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
const RUST_VERSION_RE = /^(?:stable|beta|nightly|\d+\.\d+(?:\.\d+)?)$/;
const SWIFT_ARG_RE = /^--?[A-Za-z][A-Za-z0-9-]*(?:=[A-Za-z0-9._/-]+)?$/;
const SECRETS_RE = /\bsecrets\b/i;
const UNTRUSTED_EXPRESSION_RE = /\$\{\{[^}]*(?:github\.head_ref|github\.event\.(?:pull_request|issue|comment|review|head_commit|commits|discussion|pages|workflow_run))/;
// Static expression forms allowed inside a `run:` script; everything else must travel through a quoted env variable.
const ALLOWED_RUN_EXPRESSION_RE = /^(?:matrix\.[a-z_][a-z0-9_]*|runner\.os|github\.(?:sha|run_id|run_attempt)|'[A-Za-z0-9 ._-]*')$/;
const MAX_STEPS = 40;
const MAX_RUN_CHARS = 6000;
const MAX_ITEM_JOBS = 6;
const MAX_PACKAGES = 32;

const COMMON_KEYS = ["kind", "file", "on", "concurrency", "checkout", "timeoutMinutes"];
const KIND_KEYS = {
  custom: ["runsOn", "toolchain", "steps", "artifact"],
  "swift-host": ["os", "packages", "swiftArgs", "toolchain", "artifact"],
  "dispatch-matrix": ["inputs", "plan", "jobs", "collect"],
};

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function unknownKeys(value, allowed, label, errors) {
  for (const key of Object.keys(value)) if (!allowed.includes(key)) errors.push(`unknown ${label} field: ${key}`);
}

function reservedFilenames(lanes) {
  const reserved = new Set(Object.values(LANE_FILENAMES));
  for (const lane of RUST_HYBRID_ONLY_LANES) if (LANE_FILENAMES[lane]) reserved.add(LANE_FILENAMES[lane]);
  return reserved;
}

/** Effective workflow filename: explicit `file`, else `<name>.yml`. */
export function workflowFilename(name, config) {
  return typeof config?.file === "string" ? config.file : `${name}.yml`;
}

function validateStringList(value, label, errors, { max = 16, pattern }) {
  if (!Array.isArray(value) || value.length === 0 || value.length > max
    || !value.every((item) => typeof item === "string" && pattern.test(item)) || new Set(value).size !== value.length) {
    errors.push(`${label} must be a non-empty array (at most ${max}) of unique safe values`);
    return false;
  }
  return true;
}

function validateTriggers(on, label, errors) {
  if (!isObject(on)) {
    errors.push(`${label}.on must be an object with push, pull_request and/or workflow_dispatch`);
    return;
  }
  unknownKeys(on, ["push", "pull_request", "workflow_dispatch"], `${label}.on`, errors);
  if (Object.keys(on).length === 0) errors.push(`${label}.on must declare at least one trigger`);
  for (const key of ["push", "pull_request"]) {
    const trigger = on[key];
    if (trigger === undefined || trigger === true) continue;
    if (!isObject(trigger)) {
      errors.push(`${label}.on.${key} must be true or an object with branches and/or paths`);
      continue;
    }
    unknownKeys(trigger, ["branches", "paths"], `${label}.on.${key}`, errors);
    if (trigger.branches !== undefined) validateStringList(trigger.branches, `${label}.on.${key}.branches`, errors, { pattern: BRANCH_RE });
    if (trigger.paths !== undefined) validateStringList(trigger.paths, `${label}.on.${key}.paths`, errors, { max: 32, pattern: TRIGGER_PATH_RE });
  }
  if (on.workflow_dispatch !== undefined && on.workflow_dispatch !== true) {
    errors.push(`${label}.on.workflow_dispatch must be true (declare inputs on the dispatch-matrix workflow's inputs field)`);
  }
}

function validateInputs(inputs, label, errors) {
  if (!isObject(inputs) || Object.keys(inputs).length === 0 || Object.keys(inputs).length > 10) {
    errors.push(`${label}.inputs must be an object of 1 to 10 workflow_dispatch inputs`);
    return;
  }
  for (const [name, input] of Object.entries(inputs)) {
    const at = `${label}.inputs.${name}`;
    if (!INPUT_NAME_RE.test(name)) errors.push(`${label}.inputs name must be lower snake case: ${name}`);
    if (!isObject(input)) {
      errors.push(`${at} must be an object`);
      continue;
    }
    unknownKeys(input, ["description", "required", "default", "type", "options"], at, errors);
    const type = input.type ?? "string";
    if (!["string", "boolean", "choice"].includes(type)) errors.push(`${at}.type must be string, boolean or choice`);
    if (input.description !== undefined && (typeof input.description !== "string" || !INPUT_TEXT_RE.test(input.description))) errors.push(`${at}.description must be short safe text`);
    if (input.required !== undefined && typeof input.required !== "boolean") errors.push(`${at}.required must be a boolean`);
    if (type === "boolean") {
      if (input.default !== undefined && typeof input.default !== "boolean") errors.push(`${at}.default must be a boolean`);
    } else if (input.default !== undefined && (typeof input.default !== "string" || !INPUT_VALUE_RE.test(input.default))) {
      errors.push(`${at}.default must be a short safe string`);
    }
    if (type === "choice") {
      if (!Array.isArray(input.options) || input.options.length === 0 || !input.options.every((option) => typeof option === "string" && INPUT_VALUE_RE.test(option) && option !== "")) {
        errors.push(`${at}.options must list the safe choices`);
      } else if (input.default !== undefined && !input.options.includes(input.default)) {
        errors.push(`${at}.default must be one of the options`);
      }
    } else if (input.options !== undefined) {
      errors.push(`${at}.options is only valid for type choice`);
    }
  }
}

function validateStep(step, label, errors) {
  if (!isObject(step)) {
    errors.push(`${label} must be an object`);
    return;
  }
  unknownKeys(step, ["name", "shell", "workingDirectory", "run"], label, errors);
  if (step.name !== undefined && (typeof step.name !== "string" || !STEP_NAME_RE.test(step.name))) errors.push(`${label}.name must be short safe text`);
  if (!["bash", "pwsh"].includes(step.shell)) errors.push(`${label}.shell must be bash or pwsh`);
  if (step.workingDirectory !== undefined && (typeof step.workingDirectory !== "string" || !WORK_DIR_RE.test(step.workingDirectory))) {
    errors.push(`${label}.workingDirectory must be a safe repository-relative directory or '.'`);
  }
  if (typeof step.run !== "string" || step.run.trim() === "" || step.run.length > MAX_RUN_CHARS) {
    errors.push(`${label}.run must be a non-empty script of at most ${MAX_RUN_CHARS} characters`);
    return;
  }
  if (/[\0\r]/.test(step.run)) errors.push(`${label}.run must not contain NUL or carriage-return characters`);
  if (/^\s/.test(step.run.replace(/^\n+/, ""))) errors.push(`${label}.run must not start with indentation`);
  if (SECRETS_RE.test(step.run)) errors.push(`${label}.run must not reference secrets: managed build workflows never read secrets`);
  const opened = step.run.split("${{").length - 1;
  const expressions = [...step.run.matchAll(/\$\{\{\s*([^{}]*?)\s*\}\}/g)];
  const disallowed = expressions.filter((match) => !ALLOWED_RUN_EXPRESSION_RE.test(match[1]));
  if (opened !== expressions.length || disallowed.length > 0) {
    const shown = disallowed.length > 0 ? disallowed[0][1] : "<malformed expression>";
    errors.push(`${label}.run must not interpolate \${{ ${shown} }}: only matrix.<key>, runner.os, github.sha, github.run_id, github.run_attempt and literal strings are allowed in scripts; pass any other value through a quoted env: variable (workflow_dispatch inputs already render as RIGHT_GIT_INPUT_<NAME> environment variables) and read it as "$VAR"`);
  }
  if (UNTRUSTED_EXPRESSION_RE.test(step.run)) errors.push(`${label}.run must not interpolate untrusted event data (pass it through a workflow_dispatch input, which renders as an environment variable)`);
}

function validateSteps(steps, label, errors) {
  if (!Array.isArray(steps) || steps.length === 0 || steps.length > MAX_STEPS) {
    errors.push(`${label} must be a non-empty array of at most ${MAX_STEPS} steps`);
    return;
  }
  steps.forEach((step, index) => validateStep(step, `${label}[${index}]`, errors));
}

function artifactNameTokens(name) {
  return [...name.matchAll(ARTIFACT_TOKEN_RE)].map((match) => match[1]);
}

function validateArtifact(artifact, label, errors, { requireToken }) {
  if (!isObject(artifact)) {
    errors.push(`${label} must be an object`);
    return;
  }
  unknownKeys(artifact, ["name", "path", "retentionDays", "ifNoFilesFound", "when"], label, errors);
  if (typeof artifact.name !== "string" || artifact.name.length > 120) {
    errors.push(`${label}.name must be a string`);
  } else {
    const literal = artifact.name.replace(ARTIFACT_TOKEN_RE, "");
    if (literal === "" || !ARTIFACT_LITERAL_RE.test(literal)) {
      errors.push(`${label}.name may contain only letters, digits, '.', '_', '-' and the tokens {sha}, {run_id}, {run_attempt}, {os}, {matrix.<key>}`);
    } else if (requireToken(artifactNameTokens(artifact.name)) === false) {
      errors.push(`${label}.name must carry a token that differs per job ({os} or {matrix.<key>}), or the jobs overwrite each other's artifact`);
    }
  }
  if (typeof artifact.path !== "string" || !ARTIFACT_PATH_RE.test(artifact.path)) errors.push(`${label}.path must be a safe repository-relative path or glob`);
  if (artifact.retentionDays !== undefined && (!Number.isInteger(artifact.retentionDays) || artifact.retentionDays < 1 || artifact.retentionDays > 90)) errors.push(`${label}.retentionDays must be an integer from 1 to 90`);
  if (artifact.ifNoFilesFound !== undefined && !["error", "warn", "ignore"].includes(artifact.ifNoFilesFound)) errors.push(`${label}.ifNoFilesFound must be error, warn or ignore`);
  if (artifact.when !== undefined && !["success", "always"].includes(artifact.when)) errors.push(`${label}.when must be success or always`);
}

function validateToolchain(toolchain, label, errors, { allowSwift }) {
  if (!isObject(toolchain) || Object.keys(toolchain).length === 0) {
    errors.push(`${label} must be an object with rust and/or swift`);
    return;
  }
  unknownKeys(toolchain, allowSwift ? ["rust", "swift"] : ["rust"], label, errors);
  if (toolchain.rust !== undefined && (typeof toolchain.rust !== "string" || !RUST_VERSION_RE.test(toolchain.rust))) errors.push(`${label}.rust must be stable, beta, nightly or an exact version such as 1.91.0`);
  if (toolchain.swift !== undefined) {
    const swift = toolchain.swift;
    if (!isObject(swift) || Object.keys(swift).some((key) => key !== "version" && key !== "sha256")
      || typeof swift.version !== "string" || !SWIFT_VERSION_RE.test(swift.version)
      || typeof swift.sha256 !== "string" || !SHA256_RE.test(swift.sha256)) {
      errors.push(`${label}.swift must be { version: "x.y.z", sha256: <64 lowercase hex> }; the installer's sha256 is verified before it runs`);
    }
  }
}

function validateRunsOn(runsOn, label, errors) {
  const list = typeof runsOn === "string" ? [runsOn] : runsOn;
  if (!Array.isArray(list) || list.length === 0 || list.length > 6 || !list.every((value) => typeof value === "string" && RUNNER_RE.test(value)) || new Set(list).size !== list.length) {
    errors.push(`${label} must be one safe runner label or an array of up to 6 unique labels`);
    return null;
  }
  return list;
}

function validateCustom(config, label, errors) {
  const runners = validateRunsOn(config.runsOn, `${label}.runsOn`, errors);
  if (config.toolchain !== undefined) validateToolchain(config.toolchain, `${label}.toolchain`, errors, { allowSwift: true });
  validateSteps(config.steps, `${label}.steps`, errors);
  if (config.artifact !== undefined) {
    validateArtifact(config.artifact, `${label}.artifact`, errors, { requireToken: (tokens) => !runners || runners.length < 2 || tokens.includes("os") });
  }
}

function validateSwiftHost(config, label, errors) {
  if (!Array.isArray(config.os)) errors.push(`${label}.os must be an array of runner labels`);
  const runners = Array.isArray(config.os) ? validateRunsOn(config.os, `${label}.os`, errors) : null;
  if (!validateStringList(config.packages, `${label}.packages`, errors, { max: MAX_PACKAGES, pattern: WORK_DIR_RE })) { /* reported */ }
  if (config.swiftArgs !== undefined && (!Array.isArray(config.swiftArgs) || config.swiftArgs.length > 8 || !config.swiftArgs.every((arg) => typeof arg === "string" && SWIFT_ARG_RE.test(arg)))) {
    errors.push(`${label}.swiftArgs must be an array of at most 8 plain swift test options such as "--parallel"`);
  }
  if (config.toolchain !== undefined) validateToolchain(config.toolchain, `${label}.toolchain`, errors, { allowSwift: true });
  const windows = runners ? runners.some((runner) => /^windows-/i.test(runner)) : false;
  if (windows && config.toolchain?.swift === undefined) errors.push(`${label}.toolchain.swift is required when os includes a Windows runner (Windows has no Swift toolchain)`);
  if (config.artifact !== undefined) {
    validateArtifact(config.artifact, `${label}.artifact`, errors, { requireToken: (tokens) => !runners || runners.length < 2 || tokens.includes("os") });
  }
}

function validateDispatchMatrix(config, label, errors, on) {
  if (!isObject(on) || on.workflow_dispatch !== true) errors.push(`${label}.on.workflow_dispatch must be true for kind dispatch-matrix`);
  if (config.inputs !== undefined) validateInputs(config.inputs, label, errors);
  const plan = config.plan;
  if (!isObject(plan)) {
    errors.push(`${label}.plan must be an object { run, shell, runsOn?, workingDirectory? }; the plan script writes matrix=<JSON> to $GITHUB_OUTPUT`);
  } else {
    unknownKeys(plan, ["run", "shell", "runsOn", "workingDirectory"], `${label}.plan`, errors);
    validateStep({ shell: plan.shell, run: plan.run, workingDirectory: plan.workingDirectory }, `${label}.plan`, errors);
    if (plan.runsOn !== undefined && (typeof plan.runsOn !== "string" || !RUNNER_RE.test(plan.runsOn))) errors.push(`${label}.plan.runsOn must be one safe runner label`);
  }
  if (!Array.isArray(config.jobs) || config.jobs.length === 0 || config.jobs.length > MAX_ITEM_JOBS) {
    errors.push(`${label}.jobs must be an array of 1 to ${MAX_ITEM_JOBS} per-item jobs`);
  } else {
    const ids = new Set();
    config.jobs.forEach((job, index) => {
      const at = `${label}.jobs[${index}]`;
      if (!isObject(job)) {
        errors.push(`${at} must be an object`);
        return;
      }
      unknownKeys(job, ["id", "runsOn", "toolchain", "steps", "artifact"], at, errors);
      if (typeof job.id !== "string" || !NAME_RE.test(job.id) || ["plan", "collect", "private-repo-guard"].includes(job.id)) errors.push(`${at}.id must be a lowercase job id other than plan, collect and private-repo-guard`);
      else if (ids.has(job.id)) errors.push(`${at}.id must be unique: ${job.id}`);
      else ids.add(job.id);
      if (typeof job.runsOn !== "string" || !RUNNER_RE.test(job.runsOn)) errors.push(`${at}.runsOn must be one safe runner label`);
      if (job.toolchain !== undefined) validateToolchain(job.toolchain, `${at}.toolchain`, errors, { allowSwift: true });
      validateSteps(job.steps, `${at}.steps`, errors);
      if (job.artifact !== undefined) {
        validateArtifact(job.artifact, `${at}.artifact`, errors, { requireToken: (tokens) => tokens.some((token) => token.startsWith("matrix.")) });
      }
    });
  }
  if (config.collect !== undefined) {
    const collect = config.collect;
    if (!isObject(collect)) {
      errors.push(`${label}.collect must be an object`);
      return;
    }
    unknownKeys(collect, ["runsOn", "download", "steps"], `${label}.collect`, errors);
    if (collect.runsOn !== undefined && (typeof collect.runsOn !== "string" || !RUNNER_RE.test(collect.runsOn))) errors.push(`${label}.collect.runsOn must be one safe runner label`);
    if (collect.download !== undefined) {
      if (!isObject(collect.download) || Object.keys(collect.download).some((key) => key !== "pattern" && key !== "path")
        || typeof collect.download.pattern !== "string" || !ARTIFACT_PATH_RE.test(collect.download.pattern)
        || typeof collect.download.path !== "string" || !ARTIFACT_PATH_RE.test(collect.download.path)) {
        errors.push(`${label}.collect.download must be { pattern, path } with safe values`);
      }
    }
    validateSteps(collect.steps, `${label}.collect.steps`, errors);
  }
}

/**
 * Validate manifest.workflows. Appends to `errors`; never throws.
 * Checks each workflow in isolation, then filename collisions across lanes and workflows.
 */
export function validateWorkflows(manifest, errors) {
  const workflows = manifest.workflows;
  if (workflows === undefined) return;
  if (!isObject(workflows) || Object.keys(workflows).length === 0 || Object.keys(workflows).length > 8) {
    errors.push("workflows must be an object of 1 to 8 named workflows");
    return;
  }
  const reserved = reservedFilenames(manifest.lanes);
  const files = new Map();
  for (const [name, config] of Object.entries(workflows)) {
    const label = `workflows.${name}`;
    if (!NAME_RE.test(name)) errors.push(`${label}: workflow names are lowercase letters, digits and '-' (starting with a letter)`);
    if (!isObject(config)) {
      errors.push(`${label} must be an object`);
      continue;
    }
    if (!WORKFLOW_KINDS.includes(config.kind)) {
      errors.push(`${label}.kind must be one of ${WORKFLOW_KINDS.join(", ")}`);
      continue;
    }
    unknownKeys(config, [...COMMON_KEYS, ...KIND_KEYS[config.kind]], label, errors);
    if (config.file !== undefined && (typeof config.file !== "string" || !FILE_RE.test(config.file))) {
      errors.push(`${label}.file must be a plain workflow filename ending in .yml`);
    } else {
      const file = workflowFilename(name, config);
      if (!FILE_RE.test(file)) errors.push(`${label}: the default filename ${file} is not a safe workflow filename; set file`);
      else if (reserved.has(file)) errors.push(`${label}.file ${file} collides with a built-in lane workflow`);
      else if (files.has(file)) errors.push(`${label}.file ${file} is also used by workflows.${files.get(file)}`);
      else files.set(file, name);
    }
    if (config.on === undefined && config.kind === "swift-host") {
      // swift-host defaults to push on the default branch plus pull_request.
    } else {
      validateTriggers(config.on, label, errors);
    }
    if (config.concurrency !== undefined) {
      if (!isObject(config.concurrency) || Object.keys(config.concurrency).some((key) => key !== "group" && key !== "cancelInProgress")
        || typeof config.concurrency.group !== "string" || !GROUP_RE.test(config.concurrency.group)
        || (config.concurrency.cancelInProgress !== undefined && typeof config.concurrency.cancelInProgress !== "boolean")) {
        errors.push(`${label}.concurrency must be { group: <safe prefix>, cancelInProgress?: boolean }; the group renders as <prefix>-\${{ github.ref }}`);
      }
    }
    if (config.checkout !== undefined) validateCheckout(config.checkout, errors);
    if (config.timeoutMinutes !== undefined && (!Number.isInteger(config.timeoutMinutes) || config.timeoutMinutes < 1 || config.timeoutMinutes > 360)) {
      errors.push(`${label}.timeoutMinutes must be an integer from 1 to 360`);
    }
    if (config.kind === "custom") validateCustom(config, label, errors);
    else if (config.kind === "swift-host") validateSwiftHost(config, label, errors);
    else validateDispatchMatrix(config, label, errors, config.on);
  }
}

// ---------------------------------------------------------------------------------------------- rendering

function q(value) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

function renderArtifactName(name) {
  return name.replace(ARTIFACT_TOKEN_RE, (_, token) => {
    if (token === "sha") return "${{ github.sha }}";
    if (token === "run_id") return "${{ github.run_id }}";
    if (token === "run_attempt") return "${{ github.run_attempt }}";
    if (token === "os") return "${{ matrix.os || runner.os }}";
    return `\${{ ${token} }}`;
  });
}

function pmExec(manifest) {
  const { name } = parsePackageManager(manifest.packageManager);
  return name === "pnpm" ? "pnpm exec" : name === "npm" ? "npm exec --" : "yarn exec";
}

/**
 * The `swift-toolchain` step: installs the pinned Windows Swift toolchain through
 * `right-release toolchain swift` (Orthic-Labs/rightkit#436). The installer's sha256 is verified before it runs;
 * `--github-env` exports what the installer changed to GITHUB_ENV and GITHUB_PATH for the later steps. It runs
 * on Windows only (right-release refuses other hosts; macOS runners ship Swift with Xcode). Needs
 * `@rightkit/release` in the repository's dependencies (the install step before it provides it).
 * Returns the step's lines, indented as a job step.
 */
export function swiftToolchainStep(manifest, swift) {
  return [
    "      - name: Install pinned Swift toolchain",
    "        if: runner.os == 'Windows'",
    "        shell: bash",
    `        run: ${pmExec(manifest)} right-release toolchain swift --version ${swift.version} --sha256 ${swift.sha256} --github-env`,
  ];
}

function setupLines(manifest, { linuxOwnershipGuard }) {
  const pm = parsePackageManager(manifest.packageManager);
  const lines = [
    `      - uses: ${SETUP_NODE_PIN}`,
    "        with:",
    `          node-version: ${JSON.stringify(String(manifest.matrix.node[0]))}`,
    "          package-manager-cache: false",
  ];
  if (pm.setup) lines.push(...pm.setup.split("\n"));
  lines.push(`      - run: ${pm.installCommand}`);
  const ownership = ownershipStep(manifest, "workflow").split("\n").slice(1);
  if (linuxOwnershipGuard) ownership.splice(1, 0, "        if: ${{ runner.os != 'Linux' }}");
  lines.push(...ownership);
  return lines;
}

function userStepLines(step, index) {
  const lines = [`      - name: ${q(step.name ?? `Step ${index + 1}`)}`, `        shell: ${step.shell}`];
  if (step.workingDirectory !== undefined && step.workingDirectory !== ".") lines.push(`        working-directory: ${step.workingDirectory}`);
  lines.push("        run: |");
  for (const line of step.run.replace(/^\n+/, "").replace(/\s+$/, "").split("\n")) lines.push(line === "" ? "" : `          ${line}`);
  return lines;
}

function artifactLines(artifact) {
  return [
    "      - name: Upload build artifact",
    `        if: \${{ ${artifact.when === "always" ? "always()" : "success()"} }}`,
    `        uses: ${UPLOAD_ARTIFACT_PIN}`,
    "        with:",
    `          name: ${renderArtifactName(artifact.name)}`,
    `          path: ${q(artifact.path)}`,
    `          retention-days: ${artifact.retentionDays ?? 14}`,
    `          if-no-files-found: ${artifact.ifNoFilesFound ?? "error"}`,
  ];
}

function toolchainLines(manifest, toolchain) {
  const lines = [];
  if (toolchain?.rust !== undefined) {
    lines.push(`      - uses: ${RUST_TOOLCHAIN_PIN}`, "        with:", `          toolchain: ${toolchain.rust}`);
  }
  if (toolchain?.swift !== undefined) lines.push(...swiftToolchainStep(manifest, toolchain.swift));
  return lines;
}

function inputEnvName(name) {
  return `RIGHT_GIT_INPUT_${name.toUpperCase()}`;
}

function inputEnvLines(inputs) {
  if (!inputs) return [];
  return ["    env:", ...Object.keys(inputs).map((name) => `      ${inputEnvName(name)}: \${{ inputs.${name} }}`)];
}

function runnerLinux(runner) {
  return /^(ubuntu|linux)/i.test(runner);
}

function jobLines(manifest, job) {
  const lines = [`  ${job.id}:`, `    needs: [${["private-repo-guard", ...(job.needs ?? [])].join(", ")}]`];
  if (job.condition) lines.push(`    if: ${job.condition}`);
  if (job.matrixOs) {
    lines.push("    strategy:", "      fail-fast: false", "      matrix:", `        os: ${JSON.stringify(job.matrixOs)}`, "    runs-on: ${{ matrix.os }}");
  } else if (job.matrixFromPlan) {
    lines.push("    strategy:", "      fail-fast: false", "      matrix: ${{ fromJSON(needs.plan.outputs.matrix) }}", `    runs-on: ${job.runsOn}`);
  } else {
    lines.push(`    runs-on: ${job.runsOn}`);
  }
  lines.push(`    timeout-minutes: ${job.timeoutMinutes}`);
  if (job.outputs) lines.push("    outputs:", ...Object.entries(job.outputs).map(([key, value]) => `      ${key}: ${value}`));
  lines.push(...inputEnvLines(job.inputs));
  lines.push("    steps:");
  if (job.checkout !== false) {
    lines.push(`      - uses: ${CHECKOUT_PIN}`, "        with:", "          persist-credentials: false");
  }
  if (job.download) {
    lines.push(
      "      - name: Download job artifacts",
      `        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0`,
      "        with:",
      `          pattern: ${q(job.download.pattern)}`,
      `          path: ${q(job.download.path)}`,
      "          merge-multiple: true",
    );
  }
  const linux = job.matrixOs ? job.matrixOs.some(runnerLinux) : runnerLinux(job.runsOn ?? "");
  lines.push(...setupLines(manifest, { linuxOwnershipGuard: linux }));
  lines.push(...toolchainLines(manifest, job.toolchain));
  job.steps.forEach((step, index) => lines.push(...(step.raw ?? userStepLines(step, index))));
  if (job.artifact) lines.push(...artifactLines(job.artifact));
  return lines;
}

function triggerLines(manifest, config, name) {
  const branch = manifest.defaultBranch ?? "main";
  const on = config.on ?? { push: true, pull_request: true };
  const lines = ["on:"];
  for (const key of ["push", "pull_request"]) {
    const trigger = on[key];
    if (trigger === undefined) continue;
    lines.push(`  ${key}:`);
    if (trigger === true) {
      if (key === "push") lines.push(`    branches: [${branch}]`);
      continue;
    }
    if (trigger.branches) lines.push(`    branches: [${trigger.branches.join(", ")}]`);
    if (trigger.paths) lines.push("    paths:", ...trigger.paths.map((p) => `      - ${q(p)}`));
  }
  if (on.workflow_dispatch === true) {
    lines.push("  workflow_dispatch:");
    if (config.kind === "dispatch-matrix" && config.inputs) {
      lines.push("    inputs:");
      for (const [inputName, input] of Object.entries(config.inputs)) {
        const type = input.type ?? "string";
        lines.push(`      ${inputName}:`);
        if (input.description !== undefined) lines.push(`        description: ${q(input.description)}`);
        lines.push(`        type: ${type}`);
        lines.push(`        required: ${input.required === true}`);
        if (type === "choice") lines.push("        options:", ...input.options.map((option) => `          - ${q(option)}`));
        if (input.default !== undefined) lines.push(`        default: ${type === "boolean" ? input.default : q(input.default)}`);
      }
    }
  }
  return lines;
}

function buildJobs(manifest, name, config) {
  const timeoutMinutes = config.timeoutMinutes ?? 60;
  if (config.kind === "custom") {
    const runners = typeof config.runsOn === "string" ? [config.runsOn] : config.runsOn;
    return [{
      id: "build",
      ...(runners.length > 1 ? { matrixOs: runners } : { runsOn: runners[0] }),
      timeoutMinutes,
      toolchain: config.toolchain,
      steps: config.steps,
      artifact: config.artifact,
      inputs: undefined,
    }];
  }
  if (config.kind === "swift-host") {
    const swiftArgs = config.swiftArgs?.length ? ` ${config.swiftArgs.join(" ")}` : "";
    const steps = config.packages.map((pkg) => ({ name: `swift test ${pkg}`, shell: "bash", workingDirectory: pkg, run: `swift test${swiftArgs}` }));
    return [{
      id: "swift-host",
      matrixOs: config.os,
      timeoutMinutes,
      toolchain: config.toolchain,
      steps: [{ raw: ["      - name: swift --version", "        shell: bash", "        run: swift --version"] }, ...steps],
      artifact: config.artifact,
    }];
  }
  const inputs = config.inputs;
  const jobs = [{
    id: "plan",
    runsOn: config.plan.runsOn ?? "ubuntu-24.04",
    timeoutMinutes,
    outputs: { matrix: "${{ steps.plan.outputs.matrix }}" },
    inputs,
    steps: [{ raw: planStepLines(config.plan) }],
  }];
  for (const job of config.jobs) {
    jobs.push({
      id: job.id,
      needs: ["plan"],
      runsOn: job.runsOn,
      matrixFromPlan: true,
      timeoutMinutes,
      toolchain: job.toolchain,
      steps: job.steps,
      artifact: job.artifact,
      inputs,
    });
  }
  if (config.collect) {
    jobs.push({
      id: "collect",
      needs: config.jobs.map((job) => job.id),
      condition: "${{ !cancelled() && needs.private-repo-guard.result == 'success' }}",
      runsOn: config.collect.runsOn ?? "ubuntu-24.04",
      timeoutMinutes,
      download: config.collect.download,
      steps: config.collect.steps,
      inputs,
    });
  }
  return jobs;
}

function planStepLines(plan) {
  const lines = ["      - name: Plan matrix", "        id: plan", `        shell: ${plan.shell}`];
  if (plan.workingDirectory !== undefined && plan.workingDirectory !== ".") lines.push(`        working-directory: ${plan.workingDirectory}`);
  lines.push("        run: |");
  for (const line of plan.run.replace(/^\n+/, "").replace(/\s+$/, "").split("\n")) lines.push(line === "" ? "" : `          ${line}`);
  return lines;
}

/** Render one manifest.workflows entry to workflow text. */
export function renderWorkflow(manifest, repoPath, name, config) {
  const concurrency = config.concurrency;
  const lines = [
    "# Managed by right-git — do not hand-edit. Run `right-git sync` to update.",
    `# Template: workflows.${config.kind} | Template version: ${manifest.templateVersion}`,
    `name: ${deriveProjectName(repoPath).toLowerCase().replace(/[^a-z0-9-]+/g, "-")}-${name}`,
    ...triggerLines(manifest, config, name),
    "permissions:",
    "  contents: read",
    "concurrency:",
    `  group: ${concurrency ? `${concurrency.group}-\${{ github.ref }}` : "${{ github.workflow }}-${{ github.ref }}"}`,
    `  cancel-in-progress: ${concurrency?.cancelInProgress ?? true}`,
    "jobs:",
    "  private-repo-guard:",
    "    runs-on: ubuntu-24.04",
    "    steps:",
    "      - name: Refuse private repository",
    "        if: ${{ github.event.repository.private }}",
    "        run: exit 1",
  ];
  for (const job of buildJobs(manifest, name, config)) lines.push(...jobLines(manifest, job));
  const text = `${lines.join("\n")}\n`;
  const withCheckout = { checkout: config.checkout ?? manifest.checkout };
  return applyCheckoutOptions(text, withCheckout);
}

/** Render every workflow in manifest.workflows. Returns { lane, filename, content }[] (lane is `workflow:<name>`). */
export function renderWorkflows(manifest, repoPath) {
  return Object.entries(manifest.workflows ?? {}).map(([name, config]) => ({
    lane: `workflow:${name}`,
    filename: workflowFilename(name, config),
    content: renderWorkflow(manifest, repoPath, name, config),
  }));
}
