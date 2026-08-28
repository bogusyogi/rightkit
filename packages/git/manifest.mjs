// Loader and hand-rolled validator for the per-repo `.rightgit.json` manifest.
// Mirrors the shape documented in manifest.schema.json. No ajv dependency —
// validation is plain JS so the package stays dependency-free (node builtins only).

import fs from "node:fs";
import path from "node:path";

export const MANIFEST_FILENAME = ".rightgit.json";

export const KNOWN_LANES = Object.freeze([
  "ci",
  "qualification",
  "package-smoke",
  "release-candidate",
  "publish-npm",
  "publish-package-managers",
]);

// Rendered filename for each lane, installed under <repo>/.github/workflows/.
export const LANE_FILENAMES = Object.freeze({
  ci: "ci.yml",
  qualification: "qualification.yml",
  "package-smoke": "package-smoke.yml",
  "release-candidate": "release-candidate.yml",
  "publish-npm": "publish-npm.yml",
  "publish-package-managers": "publish-package-managers.yml",
});

const PACKAGE_MANAGER_RE = /^(pnpm|npm|yarn)@\d+\.\d+\.\d+$/;
const TEMPLATE_VERSION_RE = /^\d+\.\d+\.\d+$/;

// F15 fix: every one of these fields is manifest (attacker-controlled)
// input that flows verbatim into a rendered workflow file via render.mjs's
// literal `${TOKEN}` substitution — render.mjs does no escaping, by design
// (it is a byte-deterministic template engine, not a YAML/expression-aware
// one). The re-validation proved a hostile `publish.homebrew.formulaName`
// containing GitHub Actions expression syntax (`${{ secrets.APPLE_... }}`)
// survived unmodified into rendered YAML, which GitHub Actions would then
// evaluate as a real expression — a workflow-expression injection, not just
// a cosmetic issue. The fix is at the INPUT boundary, not the render step:
// every manifest field that reaches a template must match a strict
// allowlist charset that cannot express `${{ }}`, backticks, `$()`,
// newlines, or any other shell/YAML/expression metacharacter — validated
// once here, so render.mjs can stay a dumb, deterministic string replacer.
const SAFE_MATRIX_VALUE_RE = /^[A-Za-z0-9][A-Za-z0-9._/*-]*$/;
const GITHUB_OWNER_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$/;
const GITHUB_REPO_RE = /^[A-Za-z0-9](?:[A-Za-z0-9._-]{0,99})$/;
const HOMEBREW_FORMULA_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9+_.-]*$/;
const WINGET_PACKAGE_IDENTIFIER_RE = /^[A-Za-z0-9][A-Za-z0-9-]*(?:\.[A-Za-z0-9][A-Za-z0-9-]*)+$/;

const TOP_LEVEL_KEYS = new Set([
  "schemaVersion",
  "lanes",
  "matrix",
  "packageManager",
  "publish",
  "templateVersion",
  "profile",
  "gate",
  "candidate",
]);
const GATE_PATH_RE = /^(?!.*\.\.)(?!.*\s)[A-Za-z0-9][A-Za-z0-9._/-]*$/;
const PACKAGE_SCRIPT_RE = /^[A-Za-z0-9][A-Za-z0-9:_-]*$/;

/**
 * Validate a parsed manifest object against the right-git manifest contract.
 * Never throws. Returns { valid, errors }.
 */
export function validateManifest(manifest) {
  const errors = [];

  if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
    return { valid: false, errors: ["manifest must be a JSON object"] };
  }

  for (const key of Object.keys(manifest)) {
    if (!TOP_LEVEL_KEYS.has(key)) errors.push(`unknown top-level field: ${key}`);
  }

  if (manifest.schemaVersion !== 1) {
    errors.push("schemaVersion must be exactly 1");
  }

  let lanes = [];
  if (!Array.isArray(manifest.lanes) || manifest.lanes.length === 0) {
    errors.push("lanes must be a non-empty array");
  } else {
    lanes = manifest.lanes;
    const seen = new Set();
    for (const lane of lanes) {
      if (typeof lane !== "string" || !KNOWN_LANES.includes(lane)) {
        errors.push(`unknown lane: ${JSON.stringify(lane)}`);
      } else if (seen.has(lane)) {
        errors.push(`duplicate lane: ${lane}`);
      } else {
        seen.add(lane);
      }
    }
  }

  if (manifest.matrix === null || typeof manifest.matrix !== "object" || Array.isArray(manifest.matrix)) {
    errors.push("matrix must be an object with os and node arrays");
  } else {
    for (const field of ["os", "node"]) {
      const value = manifest.matrix[field];
      if (!Array.isArray(value) || value.length === 0 || !value.every((v) => typeof v === "string" && SAFE_MATRIX_VALUE_RE.test(v))) {
        errors.push(
          `matrix.${field} must be a non-empty array of safe runner/version labels (letters, digits, '.', '_', '-', '/', '*' only — no expression or shell syntax)`,
        );
      }
    }
    for (const key of Object.keys(manifest.matrix)) {
      if (key !== "os" && key !== "node") errors.push(`unknown matrix field: ${key}`);
    }
  }

  if (typeof manifest.packageManager !== "string" || !PACKAGE_MANAGER_RE.test(manifest.packageManager)) {
    errors.push("packageManager must match '<pnpm|npm|yarn>@X.Y.Z'");
  }

  if (typeof manifest.templateVersion !== "string" || !TEMPLATE_VERSION_RE.test(manifest.templateVersion)) {
    errors.push("templateVersion must be a semver string 'X.Y.Z'");
  }

  const profile = manifest.profile === undefined ? "node" : manifest.profile;
  if (profile !== "node" && profile !== "rust-hybrid") errors.push("profile must be 'node' or 'rust-hybrid'");
  if (profile === "rust-hybrid") {
    if (lanes.some((lane) => lane !== "ci" && lane !== "release-candidate") || !lanes.includes("ci")) {
      errors.push("profile 'rust-hybrid' requires lane 'ci' and supports optional lane 'release-candidate'");
    }
    if (!manifest.gate || typeof manifest.gate !== "object" || Array.isArray(manifest.gate) || Object.keys(manifest.gate).some((key) => key !== "path") || typeof manifest.gate.path !== "string" || !GATE_PATH_RE.test(manifest.gate.path)) {
      errors.push("profile 'rust-hybrid' requires gate.path with a safe relative path");
    }
  } else if (manifest.gate !== undefined) {
    errors.push("gate is only valid for profile 'rust-hybrid'");
  }

  const candidate = manifest.candidate;
  if (candidate !== undefined) {
    if (candidate === null || typeof candidate !== "object" || Array.isArray(candidate)) {
      errors.push("candidate must be an object");
    } else {
      for (const key of Object.keys(candidate)) {
        if (!["buildScript", "checkScript", "artifactRoot"].includes(key)) errors.push(`unknown candidate field: ${key}`);
      }
      for (const key of ["buildScript", "checkScript"]) {
        if (typeof candidate[key] !== "string" || !PACKAGE_SCRIPT_RE.test(candidate[key])) {
          errors.push(`candidate.${key} must be one safe package script name`);
        }
      }
      if (typeof candidate.artifactRoot !== "string" || !GATE_PATH_RE.test(candidate.artifactRoot)) {
        errors.push("candidate.artifactRoot must be one safe relative path");
      }
    }
  }
  if (profile === "rust-hybrid" && lanes.includes("release-candidate") && candidate === undefined) {
    errors.push("rust-hybrid release-candidate lane requires candidate { buildScript, checkScript, artifactRoot }");
  }
  if (profile !== "rust-hybrid" && candidate !== undefined) {
    errors.push("candidate is only valid for profile 'rust-hybrid'");
  }

  const publish = manifest.publish;
  if (publish !== undefined) {
    if (publish === null || typeof publish !== "object" || Array.isArray(publish)) {
      errors.push("publish must be an object");
    } else {
      for (const key of Object.keys(publish)) {
        if (!["npm", "homebrew", "winget"].includes(key)) errors.push(`unknown publish field: ${key}`);
      }
      if (publish.npm !== undefined) {
        const npm = publish.npm;
        if (npm === null || typeof npm !== "object") {
          errors.push("publish.npm must be an object");
        } else {
          if (npm.oidc !== true) errors.push("publish.npm.oidc must be true (I2: OIDC is the only publish credential)");
          if (typeof npm.provenance !== "boolean") errors.push("publish.npm.provenance must be a boolean");
        }
      }
      if (publish.homebrew !== undefined) {
        const hb = publish.homebrew;
        if (hb === null || typeof hb !== "object") {
          errors.push("publish.homebrew must be an object");
        } else {
          const tapMatch = typeof hb.tap === "string" ? /^([^/]+)\/([^/]+)$/.exec(hb.tap) : null;
          if (!tapMatch || !GITHUB_OWNER_RE.test(tapMatch[1]) || !GITHUB_REPO_RE.test(tapMatch[2])) {
            errors.push(
              "publish.homebrew.tap must be 'owner/repo' using GitHub-safe characters only (letters, digits, '-', '_', '.' — no expression or shell syntax)",
            );
          }
          if (typeof hb.formulaName !== "string" || !HOMEBREW_FORMULA_NAME_RE.test(hb.formulaName)) {
            errors.push(
              "publish.homebrew.formulaName must be a Homebrew-safe identifier (letters, digits, '+', '_', '.', '-' only — no expression or shell syntax)",
            );
          }
        }
      }
      if (publish.winget !== undefined) {
        const wg = publish.winget;
        if (wg === null || typeof wg !== "object" || typeof wg.packageIdentifier !== "string" || !WINGET_PACKAGE_IDENTIFIER_RE.test(wg.packageIdentifier)) {
          errors.push(
            "publish.winget.packageIdentifier must be a dotted WinGet-safe identifier, e.g. 'Publisher.Package' (letters, digits, '-', '.' only — no expression or shell syntax)",
          );
        }
      }
    }
  }

  if (lanes.includes("publish-npm")) {
    if (!publish || !publish.npm) {
      errors.push("lane 'publish-npm' requires publish.npm { oidc: true, provenance }");
    }
  }

  if (lanes.includes("publish-package-managers")) {
    if (!publish || !publish.homebrew) {
      errors.push("lane 'publish-package-managers' requires publish.homebrew { tap, formulaName }");
    }
    if (!publish || !publish.winget) {
      errors.push("lane 'publish-package-managers' requires publish.winget { packageIdentifier }");
    }
  }

  return { valid: errors.length === 0, errors };
}

/**
 * Read and validate `<repoPath>/.rightgit.json`.
 * Never throws — returns a typed result: { ok, manifest, errors, path }.
 */
export function loadManifest(repoPath) {
  const manifestPath = path.join(repoPath, MANIFEST_FILENAME);
  let raw;
  try {
    raw = fs.readFileSync(manifestPath, "utf8");
  } catch (error) {
    return { ok: false, manifest: null, errors: [`cannot read ${manifestPath}: ${error.message}`], path: manifestPath };
  }

  let manifest;
  try {
    manifest = JSON.parse(raw);
  } catch (error) {
    return { ok: false, manifest: null, errors: [`invalid JSON in ${manifestPath}: ${error.message}`], path: manifestPath };
  }

  const { valid, errors } = validateManifest(manifest);
  if (!valid) {
    return { ok: false, manifest, errors, path: manifestPath };
  }
  return { ok: true, manifest, errors: [], path: manifestPath };
}

function parsePackageManager(spec) {
  const match = /^(pnpm|npm|yarn)@(\d+\.\d+\.\d+)$/.exec(spec);
  if (!match) throw new Error(`invalid packageManager spec: ${spec}`);
  const [, name, version] = match;
  const installCommand = {
    pnpm: "pnpm install --frozen-lockfile",
    npm: "npm ci",
    yarn: "yarn install --frozen-lockfile",
  }[name];
  const runPrefix = { pnpm: "pnpm run", npm: "npm run", yarn: "yarn run" }[name];
  const setup = {
    pnpm: `      - uses: pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86 # v6.0.10\n        with:\n          version: ${version}`,
    npm: "",
    yarn: `      - run: corepack enable && corepack prepare yarn@${version} --activate`,
  }[name];
  return { name, version, installCommand, runPrefix, setup };
}

function yamlFlowStringList(values) {
  return `[${values.map((v) => JSON.stringify(String(v))).join(", ")}]`;
}

/**
 * Derive one project name from the target repo's package.json, falling back
 * to the repo directory's basename. Never throws.
 */
export function deriveProjectName(repoPath) {
  try {
    const pkgRaw = fs.readFileSync(path.join(repoPath, "package.json"), "utf8");
    const pkg = JSON.parse(pkgRaw);
    if (typeof pkg.name === "string" && pkg.name.length > 0) {
      return pkg.name.replace(/^@[^/]+\//, "");
    }
  } catch {
    // fall through to directory name
  }
  return path.basename(path.resolve(repoPath));
}

/**
 * Build the flat placeholder -> value map used by render.mjs, from a
 * validated manifest plus the target repo's derived project name.
 */
export function buildRenderContext(manifest, repoPath) {
  const pm = parsePackageManager(manifest.packageManager);
  const projectName = deriveProjectName(repoPath);
  const artifactPrefix = projectName.toLowerCase().replace(/[^a-z0-9-]+/g, "-");

  const context = {
    OS_MATRIX: yamlFlowStringList(manifest.matrix.os),
    NODE_MATRIX: yamlFlowStringList(manifest.matrix.node),
    NODE_PUBLISH_VERSION: JSON.stringify(String(manifest.matrix.node[0])),
    PM_NAME: pm.name,
    PM_VERSION: pm.version,
    PM_CACHE: pm.name,
    INSTALL_COMMAND: pm.installCommand,
    PM_RUN: pm.runPrefix,
    PACKAGE_MANAGER_SETUP: pm.setup,
    ARTIFACT_PREFIX: artifactPrefix,
    TEMPLATE_VERSION: manifest.templateVersion,
    GATE_PATH: manifest.gate?.path,
    CANDIDATE_BUILD_SCRIPT: manifest.candidate?.buildScript,
    CANDIDATE_CHECK_SCRIPT: manifest.candidate?.checkScript,
    CANDIDATE_ARTIFACT_ROOT: manifest.candidate?.artifactRoot,
  };

  if (manifest.publish?.npm) {
    context.NPM_PROVENANCE_FLAG = manifest.publish.npm.provenance ? " --provenance" : "";
  }
  if (manifest.publish?.homebrew) {
    context.HOMEBREW_TAP = manifest.publish.homebrew.tap;
    context.HOMEBREW_FORMULA_NAME = manifest.publish.homebrew.formulaName;
  }
  if (manifest.publish?.winget) {
    context.WINGET_PACKAGE_IDENTIFIER = manifest.publish.winget.packageIdentifier;
  }

  return context;
}
