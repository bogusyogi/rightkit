// Loader and hand-rolled validator for the per-repo `.rightgit.json` manifest.
// Mirrors the shape documented in manifest.schema.json. No ajv dependency —
// validation is plain JS so the package stays dependency-free (node builtins only).

import fs from "node:fs";
import path from "node:path";
import { validateCheckout } from "./checkout.mjs";
import { validateWorkflows } from "./workflows.mjs";

export const MANIFEST_FILENAME = ".rightgit.json";

export const KNOWN_LANES = Object.freeze([
  "ci",
  "swift-check",
  "qualification",
  "package-smoke",
  "release-candidate",
  "publish-npm",
  "publish-package-managers",
  "regenerate",
]);

// Lanes only the rust-hybrid profile has a template for. They are kept out of KNOWN_LANES so the node
// profile's per-lane template checks do not look for a template it does not have.
export const RUST_HYBRID_ONLY_LANES = Object.freeze(["sign-existing"]);

// Rendered filename for each lane, installed under <repo>/.github/workflows/.
export const LANE_FILENAMES = Object.freeze({
  "sign-existing": "sign-existing.yml",
  ci: "ci.yml",
  "swift-check": "swift-check.yml",
  qualification: "qualification.yml",
  "package-smoke": "package-smoke.yml",
  "release-candidate": "release-candidate.yml",
  "publish-npm": "publish-npm.yml",
  "publish-package-managers": "publish-package-managers.yml",
  regenerate: "regenerate.yml",
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
  "compile",
  "lanes",
  "matrix",
  "packageManager",
  "publish",
  "templateVersion",
  "profile",
  "gate",
  "rustCache",
  "qaEvidencePath",
  "qaEvidenceRetentionDays",
  "ciAllOs",
  "ciIncremental",
  "concurrency",
  "devArtifact",
  "regenerate",
  "candidate",
  "ownership",
  "swift",
  "checkout",
  "defaultBranch",
  "signExisting",
  "workflows",
]);
// Optional QA evidence upload (rust-hybrid ci only). qaEvidencePath is a safe
// relative path under $RUNNER_TEMP; it renders as `${{ runner.temp }}/<path>`.
const QA_EVIDENCE_DEFAULT_RETENTION_DAYS = 7;
const QA_EVIDENCE_MAX_RETENTION_DAYS = 90;
// Pinned to the same upload-artifact release the rust-hybrid release-candidate template uses.
export const UPLOAD_ARTIFACT_PIN = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4";
const GATE_PATH_RE = /^(?!.*\.\.)(?!.*\s)[A-Za-z0-9][A-Za-z0-9._/-]*$/;
const GATE_KEYS = new Set(["path", "shards", "quick"]);
const GATE_SHARD_ID_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
const GATE_MAX_SHARDS = 16;

// gate.shards (rust-hybrid ci only): the gate runs once per shard per matrix.os runner, as
// `bash <gate.path> --group <id>`, on one shared Rust cache key, and a ci-summary job passes only when every
// leg passed. gate.quick: true adds a `quick` job (`bash <gate.path> --quick`) that every leg needs, for the
// fast deterministic checks. Steps that run inside the ci job once per runner (a non-parallel devArtifact)
// would run once per shard, so shards require devArtifact.parallel.
function validateGateShards(manifest, errors) {
  const { shards, quick } = manifest.gate;
  if (quick !== undefined && quick !== true) errors.push("gate.quick must be true when set");
  if (shards === undefined) {
    if (quick === true) errors.push("gate.quick requires gate.shards");
    return;
  }
  if (!Array.isArray(shards) || shards.length < 2 || shards.length > GATE_MAX_SHARDS) {
    errors.push(`gate.shards must list 2 to ${GATE_MAX_SHARDS} shards`);
    return;
  }
  const seen = new Set();
  for (const [index, shard] of shards.entries()) {
    if (shard === null || typeof shard !== "object" || Array.isArray(shard) || Object.keys(shard).some((key) => key !== "id") || typeof shard.id !== "string" || !GATE_SHARD_ID_RE.test(shard.id)) {
      errors.push(`gate.shards[${index}] must be { id } with a lowercase id of letters, digits and '-' (at most 32)`);
    } else if (seen.has(shard.id)) {
      errors.push(`gate.shards[${index}].id '${shard.id}' is listed twice`);
    } else {
      seen.add(shard.id);
    }
  }
  if (manifest.devArtifact !== undefined && manifest.devArtifact?.parallel !== true) {
    errors.push("gate.shards requires devArtifact.parallel: true (a dev artifact built inside the ci job would be built once per shard)");
  }
}

const gateShardIds = (manifest) => (Array.isArray(manifest.gate?.shards) ? manifest.gate.shards.map((shard) => shard.id) : []);
const RUST_WORKSPACE_PATH_RE = /^(?:\.|(?!.*\.\.)(?!.*\s)[A-Za-z0-9][A-Za-z0-9._/-]*)$/;
const PACKAGE_SCRIPT_RE = /^[A-Za-z0-9][A-Za-z0-9:_-]*$/;
const QA_SURFACE_SCRIPT_RE = /qa-native|qa-control|dev-native|dev-inspect|test-mode/i;
const GITHUB_VARIABLE_RE = /^[A-Z][A-Z0-9_]*$/;
const RELEASE_PLATFORMS = new Set(["windows", "macos", "linux"]);
const RELEASE_ARCHITECTURES = new Set(["x86_64", "arm64"]);
const NATIVE_RUNNER_TARGETS = new Map([
  ["windows-2025", ["windows", "x86_64"]],
  ["windows-11-arm", ["windows", "arm64"]],
  ["macos-15", ["macos", "arm64"]],
  ["macos-15-intel", ["macos", "x86_64"]],
]);

// Hosted `macos-*` runners plus the self-hosted Xcode runner labels (`xcode-27`).
const MACOS_RUNNER_RE = /^(macos|xcode)-/i;

function inferredTarget(os) {
  const platform = /^windows-/i.test(os) ? "windows" : MACOS_RUNNER_RE.test(os) ? "macos" : "linux";
  return { os, platform, architecture: platform === "macos" ? "arm64" : "x86_64" };
}

function candidateTargets(candidate) {
  return Array.isArray(candidate?.targets) ? candidate.targets : (candidate?.os ?? []).map(inferredTarget);
}

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
  if (manifest.compile !== "github-actions-only") {
    errors.push("compile must be exactly 'github-actions-only' for public repositories");
  }

  let lanes = [];
  if (!Array.isArray(manifest.lanes) || manifest.lanes.length === 0) {
    errors.push("lanes must be a non-empty array");
  } else {
    lanes = manifest.lanes;
    const seen = new Set();
    for (const lane of lanes) {
      if (typeof lane !== "string" || !(KNOWN_LANES.includes(lane) || RUST_HYBRID_ONLY_LANES.includes(lane))) {
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
    if (lanes.some((lane) => lane !== "ci" && lane !== "release-candidate" && lane !== "regenerate" && lane !== "sign-existing") || !lanes.includes("ci")) {
      errors.push("profile 'rust-hybrid' requires lane 'ci' and supports optional lanes 'release-candidate', 'regenerate' and 'sign-existing'");
    }
    if (!manifest.gate || typeof manifest.gate !== "object" || Array.isArray(manifest.gate) || Object.keys(manifest.gate).some((key) => !GATE_KEYS.has(key)) || typeof manifest.gate.path !== "string" || !GATE_PATH_RE.test(manifest.gate.path)) {
      errors.push("profile 'rust-hybrid' requires gate.path with a safe relative path (gate keys: path, shards, quick)");
    } else {
      validateGateShards(manifest, errors);
    }
  } else {
    if (manifest.gate !== undefined) errors.push("gate is only valid for profile 'rust-hybrid'");
    if (lanes.some((lane) => RUST_HYBRID_ONLY_LANES.includes(lane))) errors.push("lane 'sign-existing' is only valid for profile 'rust-hybrid'");
  }

  validateCheckout(manifest.checkout, errors);
  // defaultBranch: the branch the push triggers and the release-candidate admission use. Default "main".
  if (manifest.defaultBranch !== undefined
    && (typeof manifest.defaultBranch !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/.test(manifest.defaultBranch) || manifest.defaultBranch.includes(".."))) {
    errors.push("defaultBranch must be one safe branch name (letters, digits, '.', '_', '-'; no '/', whitespace, '..' or expression syntax)");
  }

  if (manifest.qaEvidencePath !== undefined) {
    if (profile !== "rust-hybrid") {
      errors.push("qaEvidencePath is only valid for profile 'rust-hybrid'");
    } else if (typeof manifest.qaEvidencePath !== "string" || !GATE_PATH_RE.test(manifest.qaEvidencePath)) {
      errors.push("qaEvidencePath must be a safe relative path under $RUNNER_TEMP (no '..', whitespace, leading '/', or expression syntax)");
    }
  }
  if (manifest.qaEvidenceRetentionDays !== undefined) {
    if (manifest.qaEvidencePath === undefined) {
      errors.push("qaEvidenceRetentionDays requires qaEvidencePath");
    } else if (!Number.isInteger(manifest.qaEvidenceRetentionDays) || manifest.qaEvidenceRetentionDays < 1 || manifest.qaEvidenceRetentionDays > QA_EVIDENCE_MAX_RETENTION_DAYS) {
      errors.push(`qaEvidenceRetentionDays must be an integer from 1 to ${QA_EVIDENCE_MAX_RETENTION_DAYS}`);
    }
  }

  // ciAllOs (rust-hybrid ci only): run the gate on every matrix.os runner
  // instead of one declared runner. Omitted or false keeps the single-runner ci job.
  if (manifest.ciAllOs !== undefined) {
    if (typeof manifest.ciAllOs !== "boolean") {
      errors.push("ciAllOs must be a boolean");
    } else if (manifest.ciAllOs === true && profile !== "rust-hybrid") {
      errors.push("ciAllOs is only valid for profile 'rust-hybrid'");
    }
  }

  // ciIncremental (rust-hybrid ci only): opt-in incremental Rust + DerivedData caching.
  if (manifest.ciIncremental !== undefined) {
    if (typeof manifest.ciIncremental !== "boolean") {
      errors.push("ciIncremental must be a boolean");
    } else if (manifest.ciIncremental === true && profile !== "rust-hybrid") {
      errors.push("ciIncremental is only valid for profile 'rust-hybrid'");
    }
  }

  if (manifest.concurrency !== undefined) validateConcurrency(manifest, lanes, errors);
  validateDevArtifact(manifest, profile, errors);
  validateWorkflows(manifest, errors);
  validateRegenerate(manifest, lanes, errors);
  validateSignExisting(manifest, lanes, errors);

  const rustCache = manifest.rustCache;
  if (rustCache !== undefined) {
    if (profile !== "rust-hybrid") {
      errors.push("rustCache is only valid for profile 'rust-hybrid'");
    } else if (rustCache === null || typeof rustCache !== "object" || Array.isArray(rustCache)) {
      errors.push("rustCache must be an object");
    } else {
      for (const key of Object.keys(rustCache)) {
        if (!["workspaces", "requiredForGate", "keySuffix"].includes(key)) errors.push(`unknown rustCache field: ${key}`);
      }
      if (!Array.isArray(rustCache.workspaces) || rustCache.workspaces.length === 0) {
        errors.push("rustCache.workspaces must be a non-empty array");
      } else if (rustCache.workspaces.some((value) => typeof value !== "string" || !RUST_WORKSPACE_PATH_RE.test(value))) {
        errors.push("rustCache.workspaces entries must be safe relative paths");
      } else if (new Set(rustCache.workspaces).size !== rustCache.workspaces.length) {
        errors.push("rustCache.workspaces entries must be unique");
      }
      if (rustCache.requiredForGate !== undefined && typeof rustCache.requiredForGate !== "boolean") {
        errors.push("rustCache.requiredForGate must be a boolean");
      }
      if (rustCache.keySuffix !== undefined && (typeof rustCache.keySuffix !== "string" || !RUST_CACHE_KEY_SUFFIX_RE.test(rustCache.keySuffix))) {
        errors.push("rustCache.keySuffix must be 1 to 64 characters: letters, digits, '.', '_', '-', starting with a letter or digit");
      }
    }
  }

  const candidate = manifest.candidate;
  if (candidate !== undefined) {
    if (candidate === null || typeof candidate !== "object" || Array.isArray(candidate)) {
      errors.push("candidate must be an object");
    } else {
      for (const key of Object.keys(candidate)) {
        if (!["buildScript", "checkScript", "admissionScript", "stageSummaryScript", "artifactRoot", "os", "targets", "signWindows", "signMacos", "releaseChain", "devQualification"].includes(key)) errors.push(`unknown candidate field: ${key}`);
      }
      // The sealed, signed candidate must be QA-free (@rightkit/release refuses
      // these surfaces in any release tier). The manifest only carries script
      // names, not the build command's features, so this can only catch a
      // script NAMED after a QA surface; the real check stays in @rightkit/release.
      for (const key of ["buildScript", "checkScript"]) {
        if (typeof candidate[key] === "string" && QA_SURFACE_SCRIPT_RE.test(candidate[key])) {
          errors.push(`candidate.${key} '${candidate[key]}' names a QA surface (qa-native, qa-control, dev-native, dev-inspect or test-mode); the signed candidate must be QA-free. Run in-app journeys against the dev build with candidate.devQualification (see Orthic-Labs/rightkit#107)`);
        }
      }
      if (candidate.devQualification !== undefined) {
        const dev = candidate.devQualification;
        if (dev === null || typeof dev !== "object" || Array.isArray(dev)) {
          errors.push("candidate.devQualification must be an object { buildScript, journeyScript }");
        } else {
          for (const key of Object.keys(dev)) {
            if (!["buildScript", "journeyScript"].includes(key)) errors.push(`unknown candidate.devQualification field: ${key}`);
          }
          for (const key of ["buildScript", "journeyScript"]) {
            if (typeof dev[key] !== "string" || !PACKAGE_SCRIPT_RE.test(dev[key])) {
              errors.push(`candidate.devQualification.${key} must be one safe package script name`);
            }
          }
          if (typeof dev.buildScript === "string" && dev.buildScript === candidate.buildScript) {
            errors.push("candidate.devQualification.buildScript must differ from candidate.buildScript: the dev build (QA surface on) is never the sealed candidate build (Orthic-Labs/rightkit#107)");
          }
        }
      }
      // admissionScript & stageSummaryScript are required for EVERY release-candidate
      // lane, not just signed/full-releaseChain ones: the tag-last workflow's
      // `admission` job (dispatch-envelope validation) and the `candidate` job's
      // stage-summary init/finalize steps are unconditional parts of the rendered
      // template, independent of candidate.signWindows/signMacos/releaseChain. If
      // either script were only required inside the optional releaseChain object,
      // an unsigned release-candidate manifest (no releaseChain at all) would still
      // render those unconditional steps with an unresolved script name.
      for (const key of ["buildScript", "checkScript", "admissionScript", "stageSummaryScript"]) {
        if (typeof candidate[key] !== "string" || !PACKAGE_SCRIPT_RE.test(candidate[key])) {
          errors.push(`candidate.${key} must be one safe package script name`);
        }
      }
      if (typeof candidate.artifactRoot !== "string" || !GATE_PATH_RE.test(candidate.artifactRoot)) {
        errors.push("candidate.artifactRoot must be one safe relative path");
      }
      if (!Array.isArray(candidate.os) || candidate.os.length === 0 || candidate.os.some((value) => typeof value !== "string" || !SAFE_MATRIX_VALUE_RE.test(value))) {
        errors.push("candidate.os must be a non-empty array of safe runner labels");
      } else if (candidate.targets === undefined && candidate.os.some((value) => !manifest.matrix?.os?.includes(value))) {
        errors.push("candidate.os entries must also exist in matrix.os");
      }
      if (candidate.targets !== undefined) {
        if (!Array.isArray(candidate.targets) || candidate.targets.length === 0) {
          errors.push("candidate.targets must be a non-empty array");
        } else {
          const targetKeys = new Set();
          for (const target of candidate.targets) {
            if (target === null || typeof target !== "object" || Array.isArray(target)) {
              errors.push("candidate.targets entries must be objects");
              continue;
            }
            if (Object.keys(target).some((key) => !["os", "platform", "architecture"].includes(key))) errors.push("candidate.targets entries may only contain os, platform, architecture");
            if (typeof target.os !== "string" || !SAFE_MATRIX_VALUE_RE.test(target.os)) errors.push("candidate.targets[].os must be a safe runner label");
            if (!RELEASE_PLATFORMS.has(target.platform)) errors.push("candidate.targets[].platform must be windows, macos, or linux");
            if (!RELEASE_ARCHITECTURES.has(target.architecture)) errors.push("candidate.targets[].architecture must be x86_64 or arm64");
            const nativeTarget = NATIVE_RUNNER_TARGETS.get(target.os);
            if (nativeTarget && (target.platform !== nativeTarget[0] || target.architecture !== nativeTarget[1])) {
              errors.push(`candidate.targets[] must declare ${target.os} as ${nativeTarget[0]}/${nativeTarget[1]}`);
            }
            if ((target.platform === "windows" && !/^windows-/i.test(target.os)) || (target.platform === "macos" && !MACOS_RUNNER_RE.test(target.os))) {
              errors.push("candidate.targets[].os must match its Windows or macOS platform");
            }
            const targetKey = `${target.os}|${target.platform}|${target.architecture}`;
            if (targetKeys.has(targetKey)) errors.push("candidate.targets entries must be unique");
            targetKeys.add(targetKey);
          }
          const targetOs = new Set(candidate.targets.map((target) => target?.os));
          if (candidate.os.some((os) => !targetOs.has(os)) || targetOs.size !== new Set(candidate.os).size || candidate.os.length !== new Set(candidate.os).size) {
            errors.push("candidate.os must contain exactly candidate.targets runner labels");
          }
        }
      }
      const targets = candidateTargets(candidate);
      if (candidate.signWindows !== undefined && typeof candidate.signWindows !== "boolean") {
        errors.push("candidate.signWindows must be a boolean");
      }
      if (candidate.signWindows === true && targets.filter((target) => target?.platform === "windows").length === 0) {
        errors.push("candidate.signWindows requires at least one Windows target");
      }
      if (candidate.signMacos !== undefined && typeof candidate.signMacos !== "boolean") {
        errors.push("candidate.signMacos must be a boolean");
      }
      if (candidate.signMacos === true && targets.filter((target) => target?.platform === "macos").length === 0) {
        errors.push("candidate.signMacos requires at least one macOS target");
      }
      if ((candidate.signWindows === true || candidate.signMacos === true) && manifest.matrix?.node?.length !== 1) {
        errors.push("candidate signing requires exactly one matrix.node version");
      }
      if (candidate.releaseChain !== undefined) {
        const chain = candidate.releaseChain;
        // admissionScript & stageSummaryScript live on candidate directly (see
        // above) — the full signed release chain adds evidenceVerificationScript
        // (runs before publish, verifying aggregated same-run evidence) plus the
        // finalize/qualify/publish scripts.
        const allowedChainKeys = ["evidenceVerificationScript", "windowsFinalizeScript", "macosFinalizeScript", "installedQualificationScript", "publishScript", "qualificationRunnerVariable", "publishSecrets", "environments"];
        if (chain === null || typeof chain !== "object" || Array.isArray(chain)) {
          errors.push("candidate.releaseChain must be an object");
        } else {
          for (const key of Object.keys(chain)) {
            if (!allowedChainKeys.includes(key)) errors.push(`unknown candidate.releaseChain field: ${key}`);
          }
          for (const key of ["evidenceVerificationScript", "windowsFinalizeScript", "installedQualificationScript", "publishScript"]) {
            if (typeof chain[key] !== "string" || !PACKAGE_SCRIPT_RE.test(chain[key])) {
              errors.push(`candidate.releaseChain.${key} must be one safe package script name`);
            }
          }
          if (chain.macosFinalizeScript !== undefined && (typeof chain.macosFinalizeScript !== "string" || !PACKAGE_SCRIPT_RE.test(chain.macosFinalizeScript))) {
          if (chain.environments !== undefined) {
            const environments = chain.environments;
            if (environments === null || typeof environments !== "object" || Array.isArray(environments)) {
              errors.push("candidate.releaseChain.environments must be an object");
            } else {
              for (const key of Object.keys(environments)) {
                if (!["signing", "qualification", "publication"].includes(key)) errors.push(`unknown candidate.releaseChain.environments field: ${key}`);
                else if (typeof environments[key] !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$/.test(environments[key])) {
                  errors.push(`candidate.releaseChain.environments.${key} must be one safe GitHub environment name`);
                }
              }
            }
          }
            errors.push("candidate.releaseChain.macosFinalizeScript must be one safe package script name");
          }
          if (typeof chain.qualificationRunnerVariable !== "string" || !GITHUB_VARIABLE_RE.test(chain.qualificationRunnerVariable)) {
            errors.push("candidate.releaseChain.qualificationRunnerVariable must be one uppercase GitHub variable name");
          }
          if (chain.publishSecrets !== undefined && (!Array.isArray(chain.publishSecrets) || new Set(chain.publishSecrets).size !== chain.publishSecrets.length || chain.publishSecrets.some((value) => typeof value !== "string" || !GITHUB_VARIABLE_RE.test(value)))) {
            errors.push("candidate.releaseChain.publishSecrets must be unique uppercase GitHub secret names");
          }
          if (candidate.signWindows !== true) errors.push("candidate.releaseChain requires candidate.signWindows=true");
          if (targets.filter((target) => target?.platform === "windows").length !== 1 || targets.filter((target) => target?.platform === "macos").length > 1) {
            errors.push("candidate.releaseChain requires at most one target per signing platform; multi-target aggregation is not supported");
          }
          if (candidate.signMacos === true && !chain.macosFinalizeScript) errors.push("candidate.releaseChain with candidate.signMacos=true requires macosFinalizeScript");
          if (chain.macosFinalizeScript && candidate.signMacos !== true) errors.push("candidate.releaseChain.macosFinalizeScript requires candidate.signMacos=true");
        }
      }
    }
  }
  if (profile === "rust-hybrid" && lanes.includes("release-candidate") && candidate === undefined) {
    errors.push("rust-hybrid release-candidate lane requires candidate { buildScript, checkScript, artifactRoot }");
  }

  if (manifest.ownership !== undefined) {
    if (!manifest.ownership || typeof manifest.ownership !== "object" || Array.isArray(manifest.ownership)
      || Object.keys(manifest.ownership).some((key) => key !== "roots")) {
      errors.push("ownership must be an object containing only roots");
    } else if (!Array.isArray(manifest.ownership.roots) || manifest.ownership.roots.length === 0
      || manifest.ownership.roots.some((root) => typeof root !== "string" || (root !== "." && !/^(?!.*\.\.)(?!.*\\)(?!.*\s)[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(root)) || root.startsWith("/"))) {
      errors.push("ownership.roots must contain safe, non-empty repository-relative paths");
    } else if (new Set(manifest.ownership.roots).size !== manifest.ownership.roots.length) {
      errors.push("ownership.roots must not contain duplicates");
    }
  }
  if (manifest.swift !== undefined) validateSwift(manifest, lanes, errors);
  if (lanes.includes("swift-check") && manifest.swift?.check === undefined) errors.push("lane 'swift-check' requires swift.check");
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

export function parsePackageManager(spec) {
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

function candidateMatrix(manifest) {
  const entries = [];
  for (const target of candidateTargets(manifest.candidate ?? { os: manifest.matrix.os })) {
    for (const node of manifest.matrix.node) entries.push({ ...target, node, targetTriple: targetTriple(target) });
  }
  return [
    "        include:",
    ...entries.flatMap((entry) => [
      `          - os: ${JSON.stringify(entry.os)}`,
      `            node: ${JSON.stringify(entry.node)}`,
      `            platform: ${JSON.stringify(entry.platform)}`,
      `            architecture: ${JSON.stringify(entry.architecture)}`,
      `            targetTriple: ${JSON.stringify(entry.targetTriple)}`,
    ]),
  ].join("\n");
}

function targetTriple(target) {
  const rustArchitecture = target.architecture === "arm64" ? "aarch64" : target.architecture;
  if (target.platform === "windows") return `${rustArchitecture}-pc-windows-msvc`;
  if (target.platform === "macos") return `${rustArchitecture}-apple-darwin`;
  return `${rustArchitecture}-unknown-linux-gnu`;
}

function signingMatrix(manifest, platform) {
  const node = String(manifest.matrix.node[0]);
  const entries = candidateTargets(manifest.candidate)
    .filter((target) => target.platform === platform)
    .map((target) => ({ ...target, node, targetTriple: targetTriple(target) }));
  return [
    "        include:",
    ...entries.flatMap((entry) => [
      `          - os: ${JSON.stringify(entry.os)}`,
      `            platform: ${JSON.stringify(entry.platform)}`,
      `            architecture: ${JSON.stringify(entry.architecture)}`,
      `            targetTriple: ${JSON.stringify(entry.targetTriple)}`,
      `            node: ${JSON.stringify(entry.node)}`,
    ]),
  ].join("\n");
}

const AZURE_LOGIN_SHA = "7ddb5af1ef8758cf1353cf3b42f940aee27ba21c"; // v3
export const DOWNLOAD_ARTIFACT_SHA = "d3f86a106a0bac45b974a628896c90dbdf5c8093"; // v4.3.0
const UPLOAD_ARTIFACT_SHA = "ea165f8d65b6e75b540449e92b4886f43607fa02"; // v4
const ARTIFACT_SIGNING_CLIENT_VERSION = "1.0.128";
const ARTIFACT_SIGNING_CLIENT_SHA256 = "74BD7D27E6CE1051409C38D9B46BC8DF0400ECD643D51FFBF2AC00869061E40B";
const WINDOWS_SDK_BUILD_TOOLS_VERSION = "10.0.26100.4188";
const WINDOWS_SDK_BUILD_TOOLS_SHA256 = "180DEB372659029864C10A0C04787833234D64AACD1D2C0661D2C00295D8E022";

// Signing and publication credentials must be reachable behind distinct
// protected environments, so a job that may sign cannot also publish. Products
// opt in per environment; absent configuration keeps the single `release`
// environment they use today, so enabling the split cannot strand secrets.
function releaseEnvironment(manifest, role) {
	const configured = manifest?.candidate?.releaseChain?.environments?.[role];
	return typeof configured === "string" && configured.length > 0 ? configured : "release";
}

// Dev artifacts (rust-hybrid ci only): fast, non-release builds on push to the default branch.
const DEV_ARTIFACT_DEFAULT_RETENTION_DAYS = 7;
const DEV_ARTIFACT_MAX_RETENTION_DAYS = 90;
const DEV_MAC_PATH_RE = /^(?!.*\.\.)[A-Za-z0-9][A-Za-z0-9._/ -]*$/;
const DEV_WINDOWS_GLOB_RE = /^(?!.*\.\.)[A-Za-z0-9][A-Za-z0-9._/*?-]*$/;
const CACHE_ACTION_PIN = "actions/cache@0400d5f644dc74513175e3cd8d07132dd4860809 # v4.2.4";

function effectiveCiRunners(manifest) {
  const os = manifest.matrix?.os;
  if (!Array.isArray(os) || os.length === 0) return [];
  if (manifest.ciAllOs === true) return os;
  return [os.find((runner) => typeof runner === "string" && runner.startsWith("windows-")) ?? os[0]];
}

// The regenerate lane is manual-only: it runs `regenerate.script`, checks the
// result, and opens a pull request from a right-git/regenerate-<run> branch.
// It never writes to the default branch.
function validateRegenerate(manifest, lanes, errors) {
  const regenerate = manifest.regenerate;
  const hasLane = Array.isArray(lanes) && lanes.includes("regenerate");
  if (regenerate === undefined) {
    if (hasLane) errors.push("lane 'regenerate' requires regenerate { script }");
    return;
  }
  if (regenerate === null || typeof regenerate !== "object" || Array.isArray(regenerate)) {
    errors.push("regenerate must be an object");
    return;
  }
  if (!hasLane) errors.push("regenerate requires lane 'regenerate'");
  for (const key of Object.keys(regenerate)) {
    if (key !== "script") errors.push(`unknown regenerate field: ${key}`);
  }
  if (typeof regenerate.script !== "string" || !PACKAGE_SCRIPT_RE.test(regenerate.script)) {
    errors.push("regenerate.script must be a package.json script name (letters, digits, ':', '_', '-')");
  }
}

// The sign-existing lane (rust-hybrid, Orthic-Labs/rightkit#257) is manual-only: it verifies an already-qualified
// artifact of another run of this repository and signs it per file with right-release sign-dev-artifact. It never builds.
const SIGN_EXISTING_ENVIRONMENT_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;

function validateSignExisting(manifest, lanes, errors) {
  const sign = manifest.signExisting;
  const hasLane = Array.isArray(lanes) && lanes.includes("sign-existing");
  if (sign === undefined) {
    if (hasLane) errors.push("lane 'sign-existing' requires signExisting { macSignManifest, macPayloadMetadata }");
    return;
  }
  if (sign === null || typeof sign !== "object" || Array.isArray(sign)) {
    errors.push("signExisting must be an object");
    return;
  }
  if (!hasLane) errors.push("signExisting requires lane 'sign-existing'");
  for (const key of Object.keys(sign)) {
    if (!["macSignManifest", "macPayloadMetadata", "macPayloadMetadataSource", "environment"].includes(key)) errors.push(`unknown signExisting field: ${key}`);
  }
  if (sign.macPayloadMetadataSource !== undefined) {
    if (typeof sign.macPayloadMetadataSource !== "string" || !GATE_PATH_RE.test(sign.macPayloadMetadataSource)) errors.push("signExisting.macPayloadMetadataSource must be a safe repository-relative path to the generated payload metadata file");
    if (typeof sign.macPayloadMetadata === "string" && sign.macPayloadMetadata.includes("/")) errors.push("signExisting.macPayloadMetadataSource places the file at the inner-zip root, so signExisting.macPayloadMetadata must be a bare file name");
    if (!["sign", "notarize", "unsigned"].includes(manifest.devArtifact?.mac)) errors.push("signExisting.macPayloadMetadataSource needs devArtifact.mac 'sign', 'notarize' or 'unsigned' (the unsigned packaging step zips it)");
  }
  if (typeof sign.macSignManifest !== "string" || !GATE_PATH_RE.test(sign.macSignManifest)) errors.push("signExisting.macSignManifest must be a safe repository-relative path to a sign manifest JSON");
  if (typeof sign.macPayloadMetadata !== "string" || !DEV_MAC_PATH_RE.test(sign.macPayloadMetadata)) errors.push("signExisting.macPayloadMetadata must be a safe path inside the artifact archive");
  if (sign.environment !== undefined && (typeof sign.environment !== "string" || !SIGN_EXISTING_ENVIRONMENT_RE.test(sign.environment))) errors.push("signExisting.environment must be one safe environment name (letters, digits, '.', '_', '-')");
  if (!Array.isArray(manifest.matrix?.os) || !manifest.matrix.os.some((runner) => typeof runner === "string" && MACOS_RUNNER_RE.test(runner))) errors.push("signExisting needs a macOS runner (macos-* or xcode-*) in matrix.os");
}

// swift.privatePackages (ci, qualification, release-candidate and regenerate lanes;
// the parallel dev-artifact-mac job): read access to private Swift package
// mirrors from a public repository's CI. The secret is a GitHub Actions secret
// name; the token value never appears in the rendered workflow.
const SWIFT_PRIVATE_MAX_ENTRIES = 4;
// Lanes whose workflows carry the configure and cleanup steps. package-smoke,
// publish-npm and publish-package-managers never do (clean-room or credentialed jobs).
const SWIFT_PRIVATE_LANES = ["ci", "qualification", "release-candidate", "regenerate", "swift-check"];
const SWIFT_PRIVATE_HOST_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,62})(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,62}))*$/;

function isSwiftPrivateSecretName(value) {
  return typeof value === "string" && /^[A-Za-z_][A-Za-z0-9_]{0,99}$/.test(value) && !/^github_/i.test(value);
}

// All fields reach shell/YAML literally: reject expressions, whitespace, traversal and option syntax.
function validateSwiftCheck(check, lanes, errors) {
  if (check === null || typeof check !== "object" || Array.isArray(check)) {
    errors.push("swift.check must be an object");
    return;
  }
  const fields = ["runner", "xcodeVersion", "projectDirectory", "spec", "project", "scheme", "xcodegenVersion"];
  for (const key of Object.keys(check)) if (!fields.includes(key)) errors.push(`unknown swift.check field: ${key}`);
  if (!lanes.includes("swift-check")) errors.push("swift.check requires lane 'swift-check'");
  if (!["macos-15", "macos-15-intel", "macos-26"].includes(check.runner)) errors.push("swift.check.runner must be a pinned GitHub-hosted macOS runner: macos-15, macos-15-intel or macos-26");
  for (const key of ["xcodeVersion", "xcodegenVersion"]) {
    const pattern = key === "xcodegenVersion" ? /^\d+\.\d+\.\d+$/ : /^\d+\.\d+(?:\.\d+)?$/;
    if (typeof check[key] !== "string" || !pattern.test(check[key])) errors.push(`swift.check.${key} must be an exact numeric version`);
  }
  if (typeof check.projectDirectory !== "string" || !RUST_WORKSPACE_PATH_RE.test(check.projectDirectory)) errors.push("swift.check.projectDirectory must be a safe relative directory or '.'");
  for (const [key, extension] of [["spec", /\.ya?ml$/], ["project", /\.xcodeproj$/]]) {
    if (typeof check[key] !== "string" || !GATE_PATH_RE.test(check[key]) || !extension.test(check[key])) errors.push(`swift.check.${key} must be a safe relative ${key === "spec" ? "YAML spec" : "xcodeproj"} path`);
  }
  if (typeof check.scheme !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(check.scheme)) errors.push("swift.check.scheme must be one safe scheme name");
}

function validateSwift(manifest, lanes, errors) {
  const swift = manifest.swift;
  if (swift === null || typeof swift !== "object" || Array.isArray(swift)) {
    errors.push("swift must be an object");
    return;
  }
  for (const key of Object.keys(swift)) {
    if (key !== "privatePackages" && key !== "check") errors.push(`unknown swift field: ${key}`);
  }
  if (swift.check !== undefined) validateSwiftCheck(swift.check, lanes, errors);
  if (swift.privatePackages === undefined && swift.check !== undefined) return;
  const entries = swift.privatePackages;
  if (!Array.isArray(entries) || entries.length === 0 || entries.length > SWIFT_PRIVATE_MAX_ENTRIES) {
    errors.push(`swift.privatePackages must be an array of 1 to ${SWIFT_PRIVATE_MAX_ENTRIES} { host, secret } entries`);
    return;
  }
  const hosts = new Set();
  entries.forEach((entry, index) => {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry) || Object.keys(entry).some((key) => key !== "host" && key !== "secret")) {
      errors.push(`swift.privatePackages[${index}] must be an object with only host and secret`);
      return;
    }
    if (typeof entry.host !== "string" || !SWIFT_PRIVATE_HOST_RE.test(entry.host)) {
      errors.push(`swift.privatePackages[${index}].host must be a bare host name such as github.com`);
    } else if (hosts.has(entry.host)) {
      errors.push(`swift.privatePackages host must be unique: ${entry.host}`);
    } else {
      hosts.add(entry.host);
    }
    if (!isSwiftPrivateSecretName(entry.secret)) {
      errors.push(`swift.privatePackages[${index}].secret must be a GitHub Actions secret name (letters, digits, '_'; not starting with a digit or GITHUB_)`);
    }
  });
  if (!Array.isArray(lanes) || !lanes.some((lane) => SWIFT_PRIVATE_LANES.includes(lane))) {
    errors.push(`swift.privatePackages requires one of the lanes ${SWIFT_PRIVATE_LANES.map((lane) => `'${lane}'`).join(", ")} (the steps render into those workflows)`);
  }
  // Dispatched runs of ci.yml can come from any ref; the private-package secret must never reach one.
  if (manifest.devArtifact?.dispatch === true) {
    errors.push("swift.privatePackages cannot be combined with devArtifact.dispatch: a manual dispatch of any ref would reach the secret");
  }
}

function validateDevArtifact(manifest, profile, errors) {
  const dev = manifest.devArtifact;
  if (dev === undefined) return;
  if (profile !== "rust-hybrid") {
    errors.push("devArtifact is only valid for profile 'rust-hybrid'");
    return;
  }
  if (dev === null || typeof dev !== "object" || Array.isArray(dev)) {
    errors.push("devArtifact must be an object");
    return;
  }
  for (const key of Object.keys(dev)) {
    if (!["mac", "windows", "retentionDays", "macAppPath", "macEntitlements", "macSignManifest", "macPayloadMetadata", "windowsGlob", "dispatch", "parallel", "buildScript"].includes(key)) errors.push(`unknown devArtifact field: ${key}`);
  }
  if (dev.mac !== undefined && !["sign", "notarize", "unsigned", "none"].includes(dev.mac)) errors.push("devArtifact.mac must be 'sign', 'notarize', 'unsigned' or 'none'");
  if (dev.windows !== undefined && dev.windows !== "unsigned" && dev.windows !== "none") errors.push("devArtifact.windows must be 'unsigned' or 'none'");
  const macSigned = dev.mac === "sign" || dev.mac === "notarize";
  const macActive = macSigned || dev.mac === "unsigned";
  if (!macActive && dev.windows !== "unsigned") errors.push("devArtifact requires mac 'sign'/'unsigned' and/or windows 'unsigned'");
  if (dev.dispatch !== undefined && dev.dispatch !== true) errors.push("devArtifact.dispatch must be true when present");
  if (dev.dispatch === true && dev.mac !== "unsigned" && dev.windows !== "unsigned") {
    errors.push("devArtifact.dispatch builds only unsigned legs; declare mac 'unsigned' and/or windows 'unsigned'");
  }
  if (dev.retentionDays !== undefined && (!Number.isInteger(dev.retentionDays) || dev.retentionDays < 1 || dev.retentionDays > DEV_ARTIFACT_MAX_RETENTION_DAYS)) {
    errors.push(`devArtifact.retentionDays must be an integer from 1 to ${DEV_ARTIFACT_MAX_RETENTION_DAYS}`);
  }
  if (dev.parallel !== undefined && typeof dev.parallel !== "boolean") errors.push("devArtifact.parallel must be a boolean");
  if (dev.buildScript !== undefined && dev.parallel !== true) errors.push("devArtifact.buildScript requires devArtifact.parallel: true");
  if (dev.parallel === true && (typeof dev.buildScript !== "string" || !PACKAGE_SCRIPT_RE.test(dev.buildScript))) {
    errors.push("devArtifact.buildScript must be one safe package script name when parallel is true (it compiles the dev app in the dev-artifact job)");
  }
  // parallel jobs run on their own platform runners, so they do not depend on ciAllOs.
  const runners = (dev.parallel === true ? (Array.isArray(manifest.matrix?.os) ? manifest.matrix.os : []) : effectiveCiRunners(manifest))
    .filter((runner) => typeof runner === "string");
  if (macActive) {
    if (typeof dev.macAppPath !== "string" || !DEV_MAC_PATH_RE.test(dev.macAppPath) || !dev.macAppPath.endsWith(".app")) {
      errors.push(`devArtifact.macAppPath must be a safe repository-relative path ending in .app when mac is '${dev.mac}'`);
    }
    if (dev.macEntitlements !== undefined && !macSigned) {
      errors.push("devArtifact.macEntitlements requires mac 'sign' or 'notarize'");
    } else if (dev.macEntitlements !== undefined && (typeof dev.macEntitlements !== "string" || !GATE_PATH_RE.test(dev.macEntitlements))) {
      errors.push("devArtifact.macEntitlements must be a safe repository-relative path");
    }
    if (dev.macSignManifest !== undefined) {
      if (dev.mac !== "sign") errors.push("devArtifact.macSignManifest requires mac 'sign' (the per-file lane never notarizes)");
      if (typeof dev.macSignManifest !== "string" || !GATE_PATH_RE.test(dev.macSignManifest)) errors.push("devArtifact.macSignManifest must be a safe repository-relative path to a sign manifest JSON");
      if (dev.macEntitlements !== undefined) errors.push("devArtifact.macSignManifest and macEntitlements cannot be combined; per-file entitlements go in the sign manifest");
      if (typeof dev.macPayloadMetadata !== "string" || !DEV_MAC_PATH_RE.test(dev.macPayloadMetadata)) errors.push("devArtifact.macPayloadMetadata (a safe path inside the staged archive) is required with macSignManifest");
    } else if (dev.macPayloadMetadata !== undefined) {
      errors.push("devArtifact.macPayloadMetadata requires macSignManifest");
    }
    if (!runners.some((runner) => MACOS_RUNNER_RE.test(runner))) {
      errors.push(dev.parallel === true
        ? `devArtifact.mac '${dev.mac}' needs a macOS runner (macos-* or xcode-*) in matrix.os`
        : `devArtifact.mac '${dev.mac}' needs the ci gate to run on a macOS runner (set ciAllOs and include a macos-* or xcode-* runner in matrix.os)`);
    }
  } else if (dev.macAppPath !== undefined || dev.macEntitlements !== undefined || dev.macSignManifest !== undefined || dev.macPayloadMetadata !== undefined) {
    errors.push("devArtifact.macAppPath, macEntitlements, macSignManifest and macPayloadMetadata require mac 'sign', 'notarize' or 'unsigned'");
  }
  if (dev.windows === "unsigned") {
    if (typeof dev.windowsGlob !== "string" || !DEV_WINDOWS_GLOB_RE.test(dev.windowsGlob)) {
      errors.push("devArtifact.windowsGlob must be a safe repository-relative glob when windows is 'unsigned'");
    }
    if (!runners.some((runner) => /^windows-/i.test(runner))) {
      errors.push(dev.parallel === true
        ? "devArtifact.windows 'unsigned' needs a windows-* runner in matrix.os"
        : "devArtifact.windows 'unsigned' needs the ci gate to run on a windows-* runner");
    }
  } else if (dev.windowsGlob !== undefined) {
    errors.push("devArtifact.windowsGlob requires windows 'unsigned'");
  }
}

const DEV_PUSH_CONDITION = "github.event_name == 'push' && github.ref_name == github.event.repository.default_branch";

// concurrency.mainPushes: "queue" stops default-branch pushes from cancelling each other.
// PR runs keep cancel-in-progress true, so superseded PR runs still cancel.
const CONCURRENCY_MAIN_PUSHES = ["cancel", "queue"];
const CI_CANCEL_MAIN_QUEUE_EXPRESSION = "${{ !(github.event_name == 'push' && github.ref_name == github.event.repository.default_branch) }}";
const CI_GROUP_PER_REF = "${{ github.workflow }}-${{ github.ref }}";
// A default-branch push gets a group of its own (never queued or cancelled at workflow level, so its
// `supersede` job always runs); every other event keeps one group per ref.
const CI_GROUP_SUPERSEDE = "${{ github.workflow }}-${{ github.event_name == 'push' && github.ref_name == github.event.repository.default_branch && github.run_id || github.ref }}";
const SUPERSEDE_SCRIPT_PATH = new URL("./supersede.mjs", import.meta.url);
const SUPERSEDE_HEREDOC_END = "RIGHT_GIT_SUPERSEDE_EOF";

/**
 * The `ci` (rust-hybrid) / `test` (node) job's own concurrency group. Default-branch pushes share one
 * group per matrix leg with cancel-in-progress false, so a newer run waits for an older run that
 * `supersede` chose to keep: never two runs in progress. Every other event gets a group unique to the
 * run, so pull requests are not serialised against main.
 */
function ciJobConcurrency(manifest = {}) {
  // With gate.shards every leg is its own concurrency slot (the shard id joins os and node).
  if (gateShardIds(manifest).length > 0) {
    return [
      "    concurrency:",
      "      group: ${{ github.event_name == 'push' && github.ref_name == github.event.repository.default_branch && format('{0}-main-{1}-{2}-{3}', github.workflow, matrix.os, matrix.node, matrix.shard) || format('{0}-{1}-{2}-{3}-{4}-{5}', github.workflow, github.run_id, github.run_attempt, matrix.os, matrix.node, matrix.shard) }}",
      "      cancel-in-progress: false",
      "",
    ].join("\n");
  }
  return [
    "    concurrency:",
    "      group: ${{ github.event_name == 'push' && github.ref_name == github.event.repository.default_branch && format('{0}-main-{1}-{2}', github.workflow, matrix.os, matrix.node) || format('{0}-{1}-{2}-{3}-{4}', github.workflow, github.run_id, github.run_attempt, matrix.os, matrix.node) }}",
    "      cancel-in-progress: false",
    "",
  ].join("\n");
}

/**
 * The `supersede` job: the only job in a managed workflow with `actions: write`. It runs the shipped
 * supersede.mjs (embedded verbatim; pure decision function tested in supersede.test.mjs) and cancels
 * older runs of this workflow on the default branch that only just started or are already failing.
 * `right-git-ci-only` in @rightkit/release accepts `actions: write` only in this exact job shape.
 */
function supersedeJob(manifest) {
  const concurrency = manifest.concurrency ?? {};
  const script = fs.readFileSync(SUPERSEDE_SCRIPT_PATH, "utf8").replace(/\r\n?/g, "\n").replace(/\n+$/, "");
  const indent = (text) => text.split("\n").map((line) => (line === "" ? "" : `          ${line}`)).join("\n");
  return [
    "  supersede:",
    "    needs: [private-repo-guard]",
    "    if: ${{ github.event_name == 'push' && github.ref_name == github.event.repository.default_branch }}",
    "    runs-on: ubuntu-24.04",
    "    timeout-minutes: 5",
    "    permissions:",
    "      contents: read",
    "      actions: write",
    "    steps:",
    "      - name: Cancel superseded runs that only just started or are failing",
    "        shell: bash",
    "        env:",
    "          GH_TOKEN: ${{ github.token }}",
    `          RIGHT_GIT_GRACE_FRACTION: "${concurrency.graceFraction ?? 0.7}"`,
    `          RIGHT_GIT_MEDIAN_WINDOW: "${concurrency.medianWindow ?? 10}"`,
    "        run: |",
    "          set -euo pipefail",
    `          cat > "$RUNNER_TEMP/supersede.mjs" <<'${SUPERSEDE_HEREDOC_END}'`,
    indent(script),
    `          ${SUPERSEDE_HEREDOC_END}`,
    '          node "$RUNNER_TEMP/supersede.mjs"',
    "",
  ].join("\n");
}
// rustCache.keySuffix: charset-checked before it reaches the rendered `key` input.
const RUST_CACHE_KEY_SUFFIX_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

function validateConcurrency(manifest, lanes, errors) {
  const concurrency = manifest.concurrency;
  if (concurrency === null || typeof concurrency !== "object" || Array.isArray(concurrency)) {
    errors.push("concurrency must be an object");
    return;
  }
  for (const key of Object.keys(concurrency)) {
    if (!["mainPushes", "reason", "graceFraction", "medianWindow"].includes(key)) errors.push(`unknown concurrency field: ${key}`);
  }
  if (concurrency.graceFraction !== undefined
    && !(typeof concurrency.graceFraction === "number" && concurrency.graceFraction > 0 && concurrency.graceFraction <= 1)) {
    errors.push("concurrency.graceFraction must be a number above 0 and at most 1");
  }
  if (concurrency.medianWindow !== undefined
    && !(Number.isInteger(concurrency.medianWindow) && concurrency.medianWindow >= 1 && concurrency.medianWindow <= 100)) {
    errors.push("concurrency.medianWindow must be an integer from 1 to 100");
  }
  if (concurrency.mainPushes === "queue" && (concurrency.graceFraction !== undefined || concurrency.medianWindow !== undefined)) {
    errors.push("concurrency.graceFraction and concurrency.medianWindow apply only to the default supersede rule, not mainPushes 'queue'");
  }
  if (concurrency.mainPushes !== undefined && !CONCURRENCY_MAIN_PUSHES.includes(concurrency.mainPushes)) {
    errors.push("concurrency.mainPushes must be 'cancel' or 'queue'");
  }
  if (concurrency.mainPushes === "queue") {
    if (typeof concurrency.reason !== "string" || concurrency.reason.trim() === "") {
      errors.push("concurrency.mainPushes 'queue' requires a non-empty concurrency.reason");
    } else if (/[\r\n\u0085\u2028\u2029]/.test(concurrency.reason)) {
      errors.push("concurrency.reason must be a single line");
    }
  } else if (concurrency.reason !== undefined) {
    errors.push("concurrency.reason is only allowed with concurrency.mainPushes 'queue'");
  }
  if (!Array.isArray(lanes) || !lanes.includes("ci")) {
    errors.push("concurrency requires lane 'ci' (it sets the ci workflow's concurrency)");
  }
}

/**
 * rustCache.keySuffix as the Swatinem/rust-cache `key` input: `<lane>-<suffix>`.
 * Returns "" when unset, so unopted renders stay byte-identical. rust-cache
 * never re-saves on an exact key hit; changing the suffix produces a new key,
 * so the next run on the default branch saves a fresh entry.
 */
function rustCacheKeyLine(manifest, lane) {
  const suffix = manifest.rustCache?.keySuffix;
  return suffix === undefined ? "" : `          key: ${lane}-${suffix}`;
}

/** ciIncremental: Xcode DerivedData cache step (leading newline; "" when unset). */
function derivedDataStep(manifest) {
  if (manifest.ciIncremental !== true) return "";
  return [
    "",
    "      - name: Cache Xcode DerivedData",
    "        if: ${{ runner.os == 'macOS' }}",
    `        uses: ${CACHE_ACTION_PIN}`,
    "        with:",
    "          path: ~/Library/Developer/Xcode/DerivedData",
    "          key: ${{ runner.os }}-${{ matrix.os }}-deriveddata-${{ hashFiles('**/Cargo.lock', '**/Package.resolved', '**/*.xcodeproj/project.pbxproj') }}",
    "          restore-keys: |",
    "            ${{ runner.os }}-${{ matrix.os }}-deriveddata-",
  ].join("\n");
}

/**
 * Dev artifact steps appended to the ci job (unsigned, no secrets) plus the
 * separate signing job. Returns "" when devArtifact is unset.
 */
// Unsigned legs may also run on a manual dispatch of any ref when
// `devArtifact.dispatch` is set; the signing job never does (push to the
// default branch only), so no secret is reachable from a dispatched run.
function devUnsignedCondition(dev) {
  return dev.dispatch === true
    ? `(${DEV_PUSH_CONDITION} || github.event_name == 'workflow_dispatch')`
    : DEV_PUSH_CONDITION;
}

export function devArtifactDispatchTrigger(manifest) {
  return manifest.devArtifact?.dispatch === true ? "  workflow_dispatch:\n" : "";
}

export function devArtifactCi(manifest, options = {}) {
  const dev = manifest.devArtifact;
  if (dev === undefined) return "";
  if (dev.parallel === true) return devArtifactParallelJobs(manifest, options);
  const retention = dev.retentionDays ?? DEV_ARTIFACT_DEFAULT_RETENTION_DAYS;
  const unsignedCondition = devUnsignedCondition(dev);
  const parts = [];
  if (dev.windows === "unsigned") {
    parts.push([
      "      - name: Upload unsigned dev build (Windows)",
      `        if: \${{ ${unsignedCondition} && runner.os == 'Windows' }}`,
      `        uses: ${UPLOAD_ARTIFACT_PIN}`,
      "        with:",
      "          name: dev-windows-${{ github.sha }}",
      `          path: ${dev.windowsGlob}`,
      `          retention-days: ${retention}`,
      "          if-no-files-found: error",
      "",
    ].join("\n"));
  }
  if (dev.mac === "sign" || dev.mac === "notarize") {
    parts.push([
      "      - name: Package unsigned dev app (macOS)",
      `        if: \${{ ${DEV_PUSH_CONDITION} && runner.os == 'macOS' }}`,
      "        shell: bash",
      "        env:",
      `          DEV_APP_PATH: ${JSON.stringify(dev.macAppPath)}`,
      "        run: |",
      "          set -euo pipefail",
      "          test -d \"$DEV_APP_PATH\"",
      "          mkdir -p \"$RUNNER_TEMP/dev-unsigned\"",
      ...devMacZipLines(manifest),
      "      - name: Upload unsigned dev app for signing (macOS)",
      `        if: \${{ ${DEV_PUSH_CONDITION} && runner.os == 'macOS' }}`,
      `        uses: ${UPLOAD_ARTIFACT_PIN}`,
      "        with:",
      "          name: dev-unsigned-mac-${{ github.sha }}",
      "          path: ${{ runner.temp }}/dev-unsigned/app.zip",
      "          retention-days: 1",
      "          if-no-files-found: error",
      "",
    ].join("\n"));
  }
  if (dev.mac === "unsigned") {
    parts.push([
      "      - name: Package unsigned dev app (macOS)",
      `        if: \${{ ${unsignedCondition} && runner.os == 'macOS' }}`,
      "        shell: bash",
      "        env:",
      `          DEV_APP_PATH: ${JSON.stringify(dev.macAppPath)}`,
      "        run: |",
      "          set -euo pipefail",
      "          test -d \"$DEV_APP_PATH\"",
      "          mkdir -p \"$RUNNER_TEMP/dev-unsigned\"",
      ...devMacZipLines(manifest),
      "      - name: Upload unsigned dev app (macOS)",
      `        if: \${{ ${unsignedCondition} && runner.os == 'macOS' }}`,
      `        uses: ${UPLOAD_ARTIFACT_PIN}`,
      "        with:",
      "          name: dev-mac-unsigned-${{ github.sha }}",
      "          path: ${{ runner.temp }}/dev-unsigned/app.zip",
      `          retention-days: ${retention}`,
      "          if-no-files-found: error",
      "",
    ].join("\n"));
  }
  let text = parts.join("");
  if (dev.mac === "sign" || dev.mac === "notarize") text += devMacSigningJob(manifest, dev, retention);
  return text;
}

// devArtifact.parallel: true. Each platform gets its own job that compiles the
// dev app (devArtifact.buildScript) and stages it. These jobs need only the
// private-repo guard, so they run beside the QA gate job instead of after it.
// Unsigned uploads therefore do not wait for the gate; the signing job still
// needs ci, so nothing is Developer ID-signed from a failed gate.
function devArtifactParallelJobs(manifest, options) {
  const dev = manifest.devArtifact;
  if (!options.artifactPrefix) throw new Error("devArtifactCi: options.artifactPrefix is required when devArtifact.parallel is true");
  const lane = options.lane ?? "ci";
  const retention = dev.retentionDays ?? DEV_ARTIFACT_DEFAULT_RETENTION_DAYS;
  const shared = { manifest, lane, artifactPrefix: options.artifactPrefix, retention };
  const parts = [];
  if (dev.mac === "sign" || dev.mac === "notarize" || dev.mac === "unsigned") parts.push(devMacArtifactJob(shared));
  if (dev.windows === "unsigned") parts.push(devWindowsArtifactJob(shared));
  if (dev.mac === "sign" || dev.mac === "notarize") parts.push(devMacSigningJob(manifest, dev, retention));
  return parts.join("");
}

function firstMatchingRunner(manifest, pattern, platform) {
  const runner = manifest.matrix.os.find((os) => typeof os === "string" && pattern.test(os));
  if (!runner) throw new Error(`devArtifact.parallel: no ${platform} runner in matrix.os`);
  return runner;
}

// The line that zips the built .app into the inner zip. With `signExisting.macPayloadMetadataSource` set, the .app (wrapper layout
// preserved, ditto) and the generated payload metadata file are staged together and zipped so the file sits at the inner-zip root
// under the basename of `macPayloadMetadata`; a missing file fails the step. Unset, the single original line renders, byte-identical.
function devMacZipLines(manifest) {
  const source = manifest.signExisting?.macPayloadMetadataSource;
  if (typeof source !== "string") return ['          ditto -c -k --keepParent "$DEV_APP_PATH" "$RUNNER_TEMP/dev-unsigned/app.zip"'];
  const name = path.posix.basename(manifest.signExisting.macPayloadMetadata);
  return [
    `          test -f ${JSON.stringify(source)}`,
    '          stage="$RUNNER_TEMP/dev-unsigned-stage"',
    '          rm -rf "$stage" && mkdir -p "$stage"',
    '          ditto "$DEV_APP_PATH" "$stage/$(basename "$DEV_APP_PATH")"',
    `          cp ${JSON.stringify(source)} "$stage/${name}"`,
    '          ditto -c -k "$stage" "$RUNNER_TEMP/dev-unsigned/app.zip"',
  ];
}

function devMacArtifactJob({ manifest, lane, artifactPrefix, retention }) {
  const dev = manifest.devArtifact;
  const signed = dev.mac === "sign" || dev.mac === "notarize";
  const stage = [
    "      - name: Package unsigned dev app (macOS)",
    "        shell: bash",
    "        env:",
    `          DEV_APP_PATH: ${JSON.stringify(dev.macAppPath)}`,
    "        run: |",
    "          set -euo pipefail",
    "          test -d \"$DEV_APP_PATH\"",
    "          mkdir -p \"$RUNNER_TEMP/dev-unsigned\"",
    ...devMacZipLines(manifest),
    ...(signed
      ? [
        "      - name: Upload unsigned dev app for signing (macOS)",
        `        uses: ${UPLOAD_ARTIFACT_PIN}`,
        "        with:",
        "          name: dev-unsigned-mac-${{ github.sha }}",
        "          path: ${{ runner.temp }}/dev-unsigned/app.zip",
        "          retention-days: 1",
        "          if-no-files-found: error",
      ]
      : [
        "      - name: Upload unsigned dev app (macOS)",
        `        uses: ${UPLOAD_ARTIFACT_PIN}`,
        "        with:",
        "          name: dev-mac-unsigned-${{ github.sha }}",
        "          path: ${{ runner.temp }}/dev-unsigned/app.zip",
        `          retention-days: ${retention}`,
        "          if-no-files-found: error",
      ]),
  ];
  return devPlatformJob({
    manifest,
    id: "dev-artifact-mac",
    label: "macOS",
    runner: firstMatchingRunner(manifest, MACOS_RUNNER_RE, "macOS"),
    condition: signed ? DEV_PUSH_CONDITION : devUnsignedCondition(dev),
    lane,
    artifactPrefix,
    stage,
    swift: true,
  });
}

function devWindowsArtifactJob({ manifest, lane, artifactPrefix, retention }) {
  const dev = manifest.devArtifact;
  const stage = [
    "      - name: Upload unsigned dev build (Windows)",
    `        uses: ${UPLOAD_ARTIFACT_PIN}`,
    "        with:",
    "          name: dev-windows-${{ github.sha }}",
    `          path: ${dev.windowsGlob}`,
    `          retention-days: ${retention}`,
    "          if-no-files-found: error",
  ];
  return devPlatformJob({
    manifest,
    id: "dev-artifact-windows",
    label: "Windows",
    runner: firstMatchingRunner(manifest, /^windows-/i, "windows-*"),
    condition: devUnsignedCondition(dev),
    lane,
    artifactPrefix,
    stage,
  });
}

// One platform job: toolchain + dependency install + dev build, then the
// platform staging steps. Mirrors the ci job's setup without the gate, the
// ownership scan, or any secret.
// swift: true renders the shared private Swift package steps around the build
// (macOS job only; the Windows job never gets them). The credential exists only
// between those two steps, so the staging and upload steps run without it.
function devPlatformJob({ manifest, id, label, runner, condition, lane, artifactPrefix, stage, swift = false }) {
  const dev = manifest.devArtifact;
  const swiftLines = (step) => (swift && step ? step.slice(1).split("\n") : []);
  const swiftConfigure = swiftLines(swiftPrivatePackagesConfigureStep(manifest));
  const swiftCleanup = swiftLines(swiftPrivatePackagesCleanupStep(manifest));
  const pm = parsePackageManager(manifest.packageManager);
  const workspaces = (manifest.rustCache?.workspaces ?? ["engine"]).map((workspace) => `            ${workspace} -> target`).join("\n");
  const cacheKey = rustCacheKeyLine(manifest, `${lane}-${id}`);
  return [
    "",
    `  ${id}:`,
    `    # ${label} dev app: builds devArtifact.buildScript and stages it. Runs beside the`,
    swiftConfigure.length > 0
      ? "    # QA gate job, not after it, and reaches only the private Swift package secret."
      : "    # QA gate job, not after it, and never reaches a secret.",
    "    needs: [private-repo-guard, check]",
    `    if: \${{ ${condition} }}`,
    `    runs-on: ${runner}`,
    "    permissions:",
    "      contents: read",
    "    steps:",
    "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1",
    "        with:",
    "          persist-credentials: false",
    "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0",
    "        with:",
    `          node-version: ${JSON.stringify(String(manifest.matrix.node[0]))}`,
    "          package-manager-cache: false",
    ...(pm.setup ? [pm.setup] : []),
    `      - run: ${pm.installCommand}`,
    "      - uses: dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0",
    "      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2",
    "        with:",
    "          workspaces: |",
    workspaces,
    `          shared-key: ${artifactPrefix}-rust`,
    ...(cacheKey ? [cacheKey] : []),
    "          add-job-id-key: false",
    `          cache-workspace-crates: ${manifest.ciIncremental === true ? "true" : "false"}`,
    "          cache-on-failure: true",
    "          save-if: true",
    ...swiftConfigure,
    "      - name: Build dev app",
    `        run: ${pm.runPrefix} ${dev.buildScript}`,
    ...swiftCleanup,
    ...stage,
    "",
  ].join("\n");
}

function devMacSigningJob(manifest, dev, retention) {
  const parallel = dev.parallel === true;
  const runner = (parallel ? manifest.matrix.os : effectiveCiRunners(manifest)).find((os) => MACOS_RUNNER_RE.test(os));
  const entitlements = dev.macEntitlements ? ` --entitlements "$GITHUB_WORKSPACE/${dev.macEntitlements}"` : "";
  // `macSignManifest`: sign each file with its fixed identifier through `right-release sign-dev-artifact`
  // (Orthic-Labs/rightkit#257) instead of `codesign --deep` on the .app. It needs the repo (the manifest) and the
  // installed right-release; unset, none of this renders and the --deep lane is byte-identical.
  const perFile = typeof dev.macSignManifest === "string";
  const pm = perFile ? parsePackageManager(manifest.packageManager) : null;
  const exec = pm ? { pnpm: "pnpm exec", npm: "npm exec --", yarn: "yarn exec" }[pm.name] : "";
  const checkout = dev.macEntitlements || perFile
    ? `      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: \${{ github.sha }}
          persist-credentials: false
${perFile ? `      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${String(manifest.matrix.node[0])}
          package-manager-cache: false
${pm.setup ? `${pm.setup}\n` : ""}      - run: ${pm.installCommand}
` : ""}`
    : "";
  const signLines = perFile
    ? `          mkdir -p "$RUNNER_TEMP/dev-signed"
          ${exec} right-release sign-dev-artifact --artifact "$work" --manifest "$GITHUB_WORKSPACE/${dev.macSignManifest}" --metadata "$work/${dev.macPayloadMetadata}" --evidence "$RUNNER_TEMP/dev-signed/sign-evidence.json"`
    : `          codesign --force --deep --strict --options runtime --timestamp${entitlements} --sign "$APPLE_DEVELOPER_ID" "$app"
          codesign --verify --deep --strict "$app"
          mkdir -p "$RUNNER_TEMP/dev-signed"`;
  const uploadPath = perFile
    ? `|
            \${{ runner.temp }}/dev-signed/*.zip
            \${{ runner.temp }}/dev-signed/sign-evidence.json`
    : "${{ runner.temp }}/dev-signed/*.zip";
  // `mac: "notarize"` also submits the signed app to the notary service and
  // staples the ticket, so app extensions (Finder Sync, Share, ...) register on
  // current macOS, which refuses them from unnotarized Developer ID apps.
  const notarize = dev.mac === "notarize";
  const apiKeyEnv = notarize ? `
          APPLE_API_KEY_BASE64: \${{ secrets.APPLE_API_KEY_BASE64 }}` : "";
  const apiKeyInstall = notarize ? `
          printf '%s' "$APPLE_API_KEY_BASE64" | base64 --decode > "$RUNNER_TEMP/AuthKey.p8"` : "";
  const notarizeStep = notarize ? `      - name: Notarize and staple dev app
        shell: bash
        env:
          APPLE_API_KEY: \${{ secrets.APPLE_API_KEY }}
          APPLE_API_ISSUER: \${{ secrets.APPLE_API_ISSUER }}
        run: |
          set -euo pipefail
          app="$(find "$RUNNER_TEMP/dev-sign" -maxdepth 1 -name '*.app' -print -quit)"
          test -n "$app"
          zip="$RUNNER_TEMP/dev-signed/$(basename "$app").zip"
          xcrun notarytool submit "$zip" --key "$RUNNER_TEMP/AuthKey.p8" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
          xcrun stapler staple "$app"
          xcrun stapler validate "$app"
          rm -f "$zip"
          ditto -c -k --keepParent "$app" "$zip"
` : "";
  const uploadLabel = notarize ? "Upload signed and notarized dev app" : "Upload signed (not notarized) dev app";
  return `
  dev-mac-sign:
    # Push to the default branch only. The release environment's protection
    # rules (required reviewers, branch policy) apply to this job.
    needs: ${parallel ? "[ci, dev-artifact-mac]" : "[ci]"}
    if: \${{ ${DEV_PUSH_CONDITION} }}
    environment: ${releaseEnvironment(manifest, "signing")}
    runs-on: ${runner}
    permissions:
      contents: read
    steps:
${checkout}      - name: Download unsigned dev app
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: dev-unsigned-mac-\${{ github.sha }}
          path: \${{ runner.temp }}/dev-unsigned
      - name: Install Apple signing identity in temporary keychain
        shell: bash
        env:
          APPLE_CERTIFICATE_BASE64: \${{ secrets.APPLE_CERTIFICATE_BASE64 }}
          APPLE_CERTIFICATE_PASSWORD: \${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_KEYCHAIN_PASSWORD: \${{ secrets.APPLE_KEYCHAIN_PASSWORD }}${apiKeyEnv}
        run: |
          set -euo pipefail
          certificate_path="$RUNNER_TEMP/apple-signing.p12"
          keychain_path="$RUNNER_TEMP/apple-signing.keychain-db"
          printf '%s' "$APPLE_CERTIFICATE_BASE64" | base64 --decode > "$certificate_path"${apiKeyInstall}
          security create-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security set-keychain-settings -lut 21600 "$keychain_path"
          security unlock-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security import "$certificate_path" -P "$APPLE_CERTIFICATE_PASSWORD" -A -t cert -f pkcs12 -k "$keychain_path"
          security set-key-partition-list -S apple-tool:,apple: -k "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security list-keychain -d user -s "$keychain_path"
      - name: Developer ID sign dev app${notarize ? "" : " (no notarization)"}
        shell: bash
        env:
          APPLE_DEVELOPER_ID: \${{ vars.APPLE_DEVELOPER_ID }}
        run: |
          set -euo pipefail
          work="$RUNNER_TEMP/dev-sign"
          rm -rf "$work" && mkdir -p "$work"
          ditto -x -k "$RUNNER_TEMP/dev-unsigned/app.zip" "$work"
          app="$(find "$work" -maxdepth 1 -name '*.app' -print -quit)"
          test -n "$app"
${signLines}
          ditto -c -k --keepParent "$app" "$RUNNER_TEMP/dev-signed/$(basename "$app").zip"
${notarizeStep}      - name: ${uploadLabel}
        uses: ${UPLOAD_ARTIFACT_PIN}
        with:
          name: dev-mac-\${{ github.sha }}
          path: ${uploadPath}
          retention-days: ${retention}
          if-no-files-found: error
      - name: Remove Apple signing material
        if: \${{ always() }}
        shell: bash
        run: |
          security delete-keychain "$RUNNER_TEMP/apple-signing.keychain-db" 2>/dev/null || true
          rm -f "$RUNNER_TEMP/apple-signing.p12"${notarize ? ' "$RUNNER_TEMP/AuthKey.p8"' : ""}
`;
}

// sign-existing lane job (Orthic-Labs/rightkit#257). Signs an already-qualified artifact of ANOTHER run; it never builds,
// never passes --release, never notarizes. Run id, artifact and source sha arrive as workflow_dispatch inputs and reach the
// shell only through env vars, after a format check. The keychain install and removal steps mirror devMacSigningJob exactly
// (same secrets, same temporary keychain); keep the two in step. Bash here avoids a braced upper-case variable, which the
// template engine would read as a placeholder.
function signExistingJob(manifest) {
  const sign = manifest.signExisting;
  const runner = manifest.matrix.os.find((os) => MACOS_RUNNER_RE.test(os));
  const pm = parsePackageManager(manifest.packageManager);
  const exec = { pnpm: "pnpm exec", npm: "npm exec --", yarn: "yarn exec" }[pm.name];
  const environment = sign.environment ?? releaseEnvironment(manifest, "signing");
  return `
  sign-existing:
    # Default branch only: the signing code and sign manifest come from this revision, and the protected
    # environment's rules (required reviewers, branch policy) apply. Nothing here compiles the app.
    needs: [private-repo-guard]
    if: \${{ github.event_name == 'workflow_dispatch' && github.ref_name == github.event.repository.default_branch }}
    environment: ${environment}
    runs-on: ${runner}
    timeout-minutes: 30
    permissions:
      contents: read
      actions: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: \${{ github.sha }}
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${String(manifest.matrix.node[0])}
          package-manager-cache: false
${pm.setup ? `${pm.setup}\n` : ""}      - run: ${pm.installCommand}${ownershipStep(manifest, "sign-existing")}
      - name: Verify the existing artifact belongs to this repository, run and source commit
        id: verify
        shell: bash
        env:
          GH_TOKEN: \${{ github.token }}
          INPUT_RUN_ID: \${{ inputs.run_id }}
          INPUT_ARTIFACT: \${{ inputs.artifact }}
          INPUT_SOURCE_SHA: \${{ inputs.source_sha }}
        run: |
          set -euo pipefail
          fail() { echo "sign-existing refused: $*" >&2; exit 1; }
          command -v jq >/dev/null || fail "jq is not available on this runner"
          [[ "$INPUT_RUN_ID" =~ ^[1-9][0-9]{0,19}$ ]] || fail "run_id must be a numeric workflow run id"
          [[ "$INPUT_ARTIFACT" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]] || fail "artifact must be a numeric artifact id or a plain artifact name"
          [[ "$INPUT_SOURCE_SHA" =~ ^[0-9a-f]{7,40}$ ]] || fail "source_sha must be 7 to 40 lowercase hexadecimal characters"
          run_json="$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$INPUT_RUN_ID")"
          run_id="$(jq -r '.id' <<<"$run_json")"
          run_repo="$(jq -r '.repository.full_name' <<<"$run_json")"
          run_repo_id="$(jq -r '.repository.id' <<<"$run_json")"
          head_repo="$(jq -r '.head_repository.full_name // empty' <<<"$run_json")"
          head_repo_id="$(jq -r '.head_repository.id // empty' <<<"$run_json")"
          head_sha="$(jq -r '.head_sha' <<<"$run_json")"
          status="$(jq -r '.status' <<<"$run_json")"
          conclusion="$(jq -r '.conclusion // empty' <<<"$run_json")"
          [ "$run_id" = "$INPUT_RUN_ID" ] || fail "run id mismatch"
          [ "$run_repo" = "$GITHUB_REPOSITORY" ] || fail "run $INPUT_RUN_ID belongs to $run_repo, not $GITHUB_REPOSITORY"
          [ "$head_repo" = "$GITHUB_REPOSITORY" ] && [ "$head_repo_id" = "$run_repo_id" ] || fail "run $INPUT_RUN_ID was built from another repository or a fork"
          [ "$status" = completed ] && [ "$conclusion" = success ] || fail "run $INPUT_RUN_ID is $status/$conclusion, not a successful completed run"
          [[ "$head_sha" =~ ^[0-9a-f]{40}$ ]] || fail "run head sha is not a full commit sha"
          case "$head_sha" in "$INPUT_SOURCE_SHA"*) ;; *) fail "run $INPUT_RUN_ID head sha $head_sha does not start with source_sha $INPUT_SOURCE_SHA" ;; esac
          artifacts_json="$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$INPUT_RUN_ID/artifacts?per_page=100" | jq -s '[.[].artifacts[]]')"
          if [[ "$INPUT_ARTIFACT" =~ ^[0-9]+$ ]]; then
            picked="$(jq -c --argjson id "$INPUT_ARTIFACT" '[.[] | select(.id == $id)]' <<<"$artifacts_json")"
          else
            picked="$(jq -c --arg name "$INPUT_ARTIFACT" '[.[] | select(.name == $name)]' <<<"$artifacts_json")"
          fi
          [ "$(jq 'length' <<<"$picked")" = 1 ] || fail "artifact $INPUT_ARTIFACT is not exactly one artifact of run $INPUT_RUN_ID"
          artifact_id="$(jq -r '.[0].id' <<<"$picked")"
          artifact_name="$(jq -r '.[0].name' <<<"$picked")"
          [ "$(jq -r '.[0].expired' <<<"$picked")" = false ] || fail "artifact $artifact_name has expired"
          [ "$(jq -r '.[0].workflow_run.id' <<<"$picked")" = "$INPUT_RUN_ID" ] || fail "artifact $artifact_name belongs to another run"
          [ "$(jq -r '.[0].workflow_run.head_sha' <<<"$picked")" = "$head_sha" ] || fail "artifact $artifact_name was built from another commit"
          [ "$(jq -r '.[0].workflow_run.head_repository_id' <<<"$picked")" = "$run_repo_id" ] || fail "artifact $artifact_name was built from another repository"
          [[ "$artifact_name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]] || fail "artifact name is not a plain name"
          git cat-file -e "$head_sha^{commit}" 2>/dev/null || fail "source commit $head_sha is not in this repository's history"
          git merge-base --is-ancestor "$head_sha" "$GITHUB_SHA" || fail "source commit $head_sha is not an ancestor of the signing revision $GITHUB_SHA"
          mkdir -p "$RUNNER_TEMP/dev-signed"
          jq -n --arg repo "$GITHUB_REPOSITORY" --arg run "$run_id" --argjson artifactId "$artifact_id" --arg artifactName "$artifact_name" --arg sourceSha "$head_sha" --arg requested "$INPUT_SOURCE_SHA" --arg signingRevision "$GITHUB_SHA" '{schema: 1, repository: $repo, runId: $run, artifactId: $artifactId, artifactName: $artifactName, sourceSha: $sourceSha, requestedSourceSha: $requested, signingRevision: $signingRevision}' > "$RUNNER_TEMP/dev-signed/source-verification.json"
          {
            printf 'run_id=%s\\n' "$run_id"
            printf 'artifact_id=%s\\n' "$artifact_id"
            printf 'artifact_name=%s\\n' "$artifact_name"
            printf 'head_sha=%s\\n' "$head_sha"
          } >> "$GITHUB_OUTPUT"
      - name: Download the verified artifact
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: \${{ steps.verify.outputs.artifact_name }}
          run-id: \${{ steps.verify.outputs.run_id }}
          repository: \${{ github.repository }}
          github-token: \${{ github.token }}
          path: \${{ runner.temp }}/existing-artifact
      - name: Install Apple signing identity in temporary keychain
        shell: bash
        env:
          APPLE_CERTIFICATE_BASE64: \${{ secrets.APPLE_CERTIFICATE_BASE64 }}
          APPLE_CERTIFICATE_PASSWORD: \${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_KEYCHAIN_PASSWORD: \${{ secrets.APPLE_KEYCHAIN_PASSWORD }}
        run: |
          set -euo pipefail
          certificate_path="$RUNNER_TEMP/apple-signing.p12"
          keychain_path="$RUNNER_TEMP/apple-signing.keychain-db"
          printf '%s' "$APPLE_CERTIFICATE_BASE64" | base64 --decode > "$certificate_path"
          security create-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security set-keychain-settings -lut 21600 "$keychain_path"
          security unlock-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security import "$certificate_path" -P "$APPLE_CERTIFICATE_PASSWORD" -A -t cert -f pkcs12 -k "$keychain_path"
          security set-key-partition-list -S apple-tool:,apple: -k "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security list-keychain -d user -s "$keychain_path"
      - name: Developer ID sign existing artifact (no rebuild, no notarization)
        shell: bash
        env:
          APPLE_DEVELOPER_ID: \${{ vars.APPLE_DEVELOPER_ID }}
        run: |
          set -euo pipefail
          downloaded="$RUNNER_TEMP/existing-artifact"
          work="$RUNNER_TEMP/dev-sign"
          rm -rf "$work" && mkdir -p "$work" "$RUNNER_TEMP/dev-signed"
          zips=()
          while IFS= read -r -d '' file; do zips+=("$file"); done < <(find "$downloaded" -type f -name '*.zip' -print0)
          if [ "\${#zips[@]}" -eq 1 ]; then
            ditto -x -k "\${zips[0]}" "$work"
            out="$(basename "\${zips[0]}")"
          elif [ "\${#zips[@]}" -eq 0 ]; then
            ditto "$downloaded" "$work"
            out="signed-artifact.zip"
          else
            echo "sign-existing refused: the artifact holds more than one zip" >&2
            exit 1
          fi
          ${exec} right-release sign-dev-artifact --artifact "$work" --manifest "$GITHUB_WORKSPACE/${sign.macSignManifest}" --metadata "$work/${sign.macPayloadMetadata}" --evidence "$RUNNER_TEMP/dev-signed/sign-evidence.json"
          ditto -c -k "$work" "$RUNNER_TEMP/dev-signed/$out"
      - name: Upload signed existing artifact
        uses: ${UPLOAD_ARTIFACT_PIN}
        with:
          name: signed-\${{ steps.verify.outputs.artifact_name }}-\${{ steps.verify.outputs.head_sha }}
          path: |
            \${{ runner.temp }}/dev-signed/*.zip
            \${{ runner.temp }}/dev-signed/sign-evidence.json
            \${{ runner.temp }}/dev-signed/source-verification.json
          retention-days: ${DEV_ARTIFACT_DEFAULT_RETENTION_DAYS}
          if-no-files-found: error
      - name: Remove Apple signing material
        if: \${{ always() }}
        shell: bash
        run: |
          security delete-keychain "$RUNNER_TEMP/apple-signing.keychain-db" 2>/dev/null || true
          rm -f "$RUNNER_TEMP/apple-signing.p12"
`;
}

// A configured dev-qualification job gates both signing jobs: a failed or
// skipped dev qualification leaves signing skipped (neither signing job uses
// always()). Unconfigured, the list is byte-identical to the original.
function signingNeeds(manifest) {
  return manifest.candidate?.devQualification ? "[admission, candidate, dev-qualification]" : "[admission, candidate]";
}

// Optional lane (Orthic-Labs/rightkit#107): builds the DEV build (QA surface on)
// from the same admitted source_revision and runs the app's in-app journeys
// against it. Never touches candidate or signed bytes; the signed candidate stays
// QA-free and is qualified from outside by installed-qualification.
function devQualificationJob(manifest, pm, artifactPrefix, cacheKeyLine) {
  const dev = manifest.candidate?.devQualification;
  if (!dev) return "";
  const root = `\${{ runner.temp }}/${manifest.candidate.artifactRoot}-devqual`;
  const identity = `\${{ needs.admission.outputs.version }}-\${{ needs.admission.outputs.source_revision }}\${{ needs.admission.outputs.artifact_suffix }}`;
  const stageEnv = (action, extra = "") => `          RIGHT_GIT_STAGE_ACTION: ${action}
          RIGHT_GIT_STAGE_ROOT: \${{ runner.temp }}/right-git-stage-summary
          RIGHT_GIT_STAGE_EVIDENCE_ROOT: ${root}
          RIGHT_GIT_STAGE: dev-qualification
          RIGHT_GIT_STAGE_PRODUCER: dev-qualification-\${{ matrix.platform }}-\${{ matrix.architecture }}${extra}
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          RIGHT_GIT_RUN_ID: \${{ github.run_id }}
          RIGHT_GIT_RUN_ATTEMPT: \${{ github.run_attempt }}`;
  const stepEnv = `          RIGHT_GIT_ARTIFACT_ROOT: ${root}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}`;
  return `
  dev-qualification:
    needs: [admission]
    env:
      SCCACHE_GHA_ENABLED: "true"
      RUSTC_WRAPPER: "sccache"
      RIGHTKIT_DEV_SIGNING: "ci-adhoc"
    strategy:
      fail-fast: false
      matrix:
${candidateMatrix(manifest)}
    runs-on: \${{ matrix.os }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: \${{ needs.admission.outputs.source_revision }}
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: \${{ matrix.node }}
          package-manager-cache: false
${pm.setup}
      - uses: dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0
        with:
          components: rustfmt, clippy
          targets: \${{ matrix.targetTriple }}
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          workspaces: |
${(manifest.rustCache?.workspaces ?? ["engine"]).map((workspace) => `            ${workspace} -> target`).join("\n")}
          shared-key: ${artifactPrefix}-rust
          add-job-id-key: false
${cacheKeyLine ? `${cacheKeyLine}\n` : ""}          cache-workspace-crates: false
          cache-on-failure: true
          save-if: true
      - uses: Mozilla-Actions/sccache-action@fc920bf0ec8de6ee65d409111f7ec508035751ba # v0.0.11
        with:
          version: "v0.16.0"
      - run: ${pm.installCommand}
      - name: Initialize dev-qualification stage summary
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
${stageEnv("init")}${swiftPrivatePackagesConfigureStep(manifest)}
      - name: Build dev build (QA surface on, never signed)
        run: ${pm.runPrefix} ${dev.buildScript}
        env:
${stepEnv}
      - name: Run in-app journeys against the dev build
        run: ${pm.runPrefix} ${dev.journeyScript}
        env:
${stepEnv}${swiftPrivatePackagesCleanupStep(manifest)}
      - name: Finalize dev-qualification stage summary
        if: \${{ always() }}
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
${stageEnv("finalize", `
          RIGHT_GIT_STAGE_STATUS: \${{ job.status == 'success' && 'SUCCEEDED' || 'FAILED' }}
          RIGHT_GIT_STAGE_EXIT_CODE: \${{ job.status == 'success' && '0' || '1' }}`)}
      - name: Upload dev-qualification evidence
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-devqual-\${{ matrix.platform }}-\${{ matrix.architecture }}-${identity}
          path: ${root}/**
          if-no-files-found: error
      - name: Upload dev-qualification stage summary
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-dev-qualification-\${{ matrix.platform }}-\${{ matrix.architecture }}-${identity}
          path: \${{ runner.temp }}/right-git-stage-summary/stage-summary.json
          if-no-files-found: error
`;
}

function windowsSigningJob(manifest, pm, artifactPrefix) {
  if (manifest.candidate?.signWindows !== true) return "";
  const nodeVersion = String(manifest.matrix.node[0]);
  const releaseChain = manifest.candidate.releaseChain;
  const finalizeCommand = releaseChain
    ? `${pm.runPrefix} ${releaseChain.windowsFinalizeScript}`
    : "pnpm exec right-release build --platform win --skip-checks";
  const finalizeEnvironment = releaseChain
    ? `          RIGHT_GIT_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-windows-candidate
          RIGHT_GIT_FINALIZED_WINDOWS_ROOT: \${{ runner.temp }}/finalized-windows
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}`
    : `          LEGION_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-windows-candidate
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
          LEGION_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}`;
  // Orthic-Labs/rightkit#183: a source QA receipt carried in the candidate handoff (`qa/source.receipt.json`, written by
  // `right-release qa --stage source --carry-to <artifact root>/qa`) is read by `right-release build`. Absent file: no effect.
  const qaReceiptEnvironment = `\n          RIGHT_RELEASE_QA_RECEIPT: \${{ runner.temp }}/unsigned-windows-candidate/qa`;
  const uploadPath = releaseChain
    ? "            ${{ runner.temp }}/finalized-windows/**"
    : "            dist/releases/windows/**\n            .right-release/receipts/windows-*.json";
  return `
  windows-sign:
    needs: ${signingNeeds(manifest)}
    if: \${{ needs.admission.outputs.signed_qualification == 'true' }}
    environment: ${releaseEnvironment(manifest, "signing")}
    strategy:
      fail-fast: false
      matrix:
${signingMatrix(manifest, "windows")}
    runs-on: \${{ matrix.os }}
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: \${{ needs.admission.outputs.source_revision }}
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${nodeVersion}
          package-manager-cache: false
${pm.setup}
      - uses: dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0
        with:
          targets: \${{ matrix.targetTriple }}
      - run: ${pm.installCommand}
      - name: Initialize Windows signing stage summary
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
          RIGHT_GIT_STAGE_ACTION: init
          RIGHT_GIT_STAGE_ROOT: \${{ runner.temp }}/right-git-stage-summary
          RIGHT_GIT_STAGE_EVIDENCE_ROOT: \${{ runner.temp }}/finalized-windows
          RIGHT_GIT_STAGE: windows-sign
          RIGHT_GIT_STAGE_PRODUCER: windows-sign-\${{ matrix.architecture }}
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          RIGHT_GIT_RUN_ID: \${{ github.run_id }}
          RIGHT_GIT_RUN_ATTEMPT: \${{ github.run_attempt }}
      - name: Download exact unsigned Windows candidate
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-candidate-\${{ matrix.platform }}-\${{ matrix.architecture }}-\${{ needs.admission.outputs.version }}-\${{ needs.admission.outputs.source_revision }}\${{ needs.admission.outputs.artifact_suffix }}
          path: \${{ runner.temp }}/unsigned-windows-candidate
      - name: Authenticate to Azure with GitHub OIDC
        uses: azure/login@${AZURE_LOGIN_SHA} # v3
        with:
          client-id: \${{ vars.AZURE_CLIENT_ID }}
          tenant-id: \${{ vars.AZURE_TENANT_ID }}
          subscription-id: \${{ vars.AZURE_SUBSCRIPTION_ID }}
      - name: Install pinned Artifact Signing client
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          $tools = Join-Path $env:RUNNER_TEMP 'artifact-signing-tools'
          New-Item -ItemType Directory -Force -Path $tools | Out-Null
          $client = Join-Path $tools 'client.zip'
          $sdk = Join-Path $tools 'sdk.zip'
          Invoke-WebRequest 'https://api.nuget.org/v3-flatcontainer/microsoft.artifactsigning.client/${ARTIFACT_SIGNING_CLIENT_VERSION}/microsoft.artifactsigning.client.${ARTIFACT_SIGNING_CLIENT_VERSION}.nupkg' -OutFile $client
          Invoke-WebRequest 'https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.buildtools/${WINDOWS_SDK_BUILD_TOOLS_VERSION}/microsoft.windows.sdk.buildtools.${WINDOWS_SDK_BUILD_TOOLS_VERSION}.nupkg' -OutFile $sdk
          if ((Get-FileHash $client -Algorithm SHA256).Hash -ne '${ARTIFACT_SIGNING_CLIENT_SHA256}') { throw 'Artifact Signing client hash mismatch' }
          if ((Get-FileHash $sdk -Algorithm SHA256).Hash -ne '${WINDOWS_SDK_BUILD_TOOLS_SHA256}') { throw 'Windows SDK Build Tools hash mismatch' }
          Expand-Archive $client (Join-Path $tools 'client')
          Expand-Archive $sdk (Join-Path $tools 'sdk')
          "AZURE_ARTIFACT_SIGNING_DLIB_PATH=$tools\\client\\bin\\x64\\Azure.CodeSigning.Dlib.dll" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
          "AZURE_SIGNTOOL_PATH=$tools\\sdk\\bin\\10.0.26100.0\\x64\\signtool.exe" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
      - name: Sign & finalize Windows candidate
        run: ${finalizeCommand}
        env:
${finalizeEnvironment}${qaReceiptEnvironment}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          AZURE_ARTIFACT_SIGNING_ENDPOINT: \${{ vars.AZURE_ARTIFACT_SIGNING_ENDPOINT }}
          AZURE_ARTIFACT_SIGNING_ACCOUNT: \${{ vars.AZURE_ARTIFACT_SIGNING_ACCOUNT }}
          AZURE_ARTIFACT_SIGNING_PROFILE: \${{ vars.AZURE_ARTIFACT_SIGNING_PROFILE }}
      - name: Finalize Windows signing stage summary
        if: \${{ always() }}
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
          RIGHT_GIT_STAGE_ACTION: finalize
          RIGHT_GIT_STAGE_ROOT: \${{ runner.temp }}/right-git-stage-summary
          RIGHT_GIT_STAGE_EVIDENCE_ROOT: \${{ runner.temp }}/finalized-windows
          RIGHT_GIT_STAGE: windows-sign
          RIGHT_GIT_STAGE_PRODUCER: windows-sign-\${{ matrix.architecture }}
          RIGHT_GIT_STAGE_STATUS: \${{ job.status == 'success' && 'SUCCEEDED' || 'FAILED' }}
          RIGHT_GIT_STAGE_EXIT_CODE: \${{ job.status == 'success' && '0' || '1' }}
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          RIGHT_GIT_RUN_ID: \${{ github.run_id }}
          RIGHT_GIT_RUN_ATTEMPT: \${{ github.run_attempt }}
      - name: Upload signed Windows candidate
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-signed-windows-\${{ matrix.architecture }}-\${{ needs.admission.outputs.version }}-\${{ needs.admission.outputs.source_revision }}\${{ needs.admission.outputs.artifact_suffix }}
          path: |
${uploadPath}
          if-no-files-found: error
      - name: Upload Windows signing stage summary
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-windows-sign-\${{ matrix.architecture }}-\${{ needs.admission.outputs.version }}-\${{ needs.admission.outputs.source_revision }}\${{ needs.admission.outputs.artifact_suffix }}
          path: \${{ runner.temp }}/right-git-stage-summary/stage-summary.json
          if-no-files-found: error
`.trimEnd();
}

function macosSigningJob(manifest, pm, artifactPrefix) {
  if (manifest.candidate?.signMacos !== true) return "";
  const nodeVersion = String(manifest.matrix.node[0]);
  const releaseChain = manifest.candidate.releaseChain;
  const finalizeCommand = releaseChain
    ? `${pm.runPrefix} ${releaseChain.macosFinalizeScript}`
    : "pnpm exec right-release build --platform mac --skip-checks";
  const finalizeEnvironment = releaseChain
    ? `          RIGHT_GIT_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-macos-candidate
          RIGHT_GIT_FINALIZED_MACOS_ROOT: \${{ runner.temp }}/finalized-macos
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}`
    : `          LEGION_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-macos-candidate
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
          LEGION_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}`;
  const qaReceiptEnvironment = `\n          RIGHT_RELEASE_QA_RECEIPT: \${{ runner.temp }}/unsigned-macos-candidate/qa`;
  const uploadPath = releaseChain
    ? "            ${{ runner.temp }}/finalized-macos/**"
    : "            dist/releases/mac/**\n            .right-release/receipts/macos-*.json";
  return `
  macos-sign:
    needs: ${signingNeeds(manifest)}
    if: \${{ needs.admission.outputs.signed_qualification == 'true' }}
    environment: ${releaseEnvironment(manifest, "signing")}
    strategy:
      fail-fast: false
      matrix:
${signingMatrix(manifest, "macos")}
    runs-on: \${{ matrix.os }}
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: \${{ needs.admission.outputs.source_revision }}
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${nodeVersion}
          package-manager-cache: false
${pm.setup}
      - uses: dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0
        with:
          targets: \${{ matrix.targetTriple }}
      - run: ${pm.installCommand}
      - name: Initialize macOS signing stage summary
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
          RIGHT_GIT_STAGE_ACTION: init
          RIGHT_GIT_STAGE_ROOT: \${{ runner.temp }}/right-git-stage-summary
          RIGHT_GIT_STAGE_EVIDENCE_ROOT: \${{ runner.temp }}/finalized-macos
          RIGHT_GIT_STAGE: macos-sign
          RIGHT_GIT_STAGE_PRODUCER: macos-sign-\${{ matrix.architecture }}
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          RIGHT_GIT_RUN_ID: \${{ github.run_id }}
          RIGHT_GIT_RUN_ATTEMPT: \${{ github.run_attempt }}
      - name: Download exact unsigned macOS candidate
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-candidate-\${{ matrix.platform }}-\${{ matrix.architecture }}-\${{ needs.admission.outputs.version }}-\${{ needs.admission.outputs.source_revision }}\${{ needs.admission.outputs.artifact_suffix }}
          path: \${{ runner.temp }}/unsigned-macos-candidate
      - name: Install Apple signing identity in temporary keychain
        shell: bash
        env:
          APPLE_CERTIFICATE_BASE64: \${{ secrets.APPLE_CERTIFICATE_BASE64 }}
          APPLE_CERTIFICATE_PASSWORD: \${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_KEYCHAIN_PASSWORD: \${{ secrets.APPLE_KEYCHAIN_PASSWORD }}
          APPLE_API_KEY_BASE64: \${{ secrets.APPLE_API_KEY_BASE64 }}
        run: |
          set -euo pipefail
          certificate_path="$RUNNER_TEMP/apple-signing.p12"
          key_path="$RUNNER_TEMP/AuthKey.p8"
          keychain_path="$RUNNER_TEMP/apple-signing.keychain-db"
          printf '%s' "$APPLE_CERTIFICATE_BASE64" | base64 --decode > "$certificate_path"
          printf '%s' "$APPLE_API_KEY_BASE64" | base64 --decode > "$key_path"
          security create-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security set-keychain-settings -lut 21600 "$keychain_path"
          security unlock-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security import "$certificate_path" -P "$APPLE_CERTIFICATE_PASSWORD" -A -t cert -f pkcs12 -k "$keychain_path"
          security set-key-partition-list -S apple-tool:,apple: -k "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security list-keychain -d user -s "$keychain_path"
          printf 'APPLE_API_KEY_PATH=%s\\n' "$key_path" >> "$GITHUB_ENV"
      - name: Sign, notarize & finalize macOS candidate
        run: ${finalizeCommand}
        env:
${finalizeEnvironment}${qaReceiptEnvironment}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          APPLE_DEVELOPER_ID: \${{ vars.APPLE_DEVELOPER_ID }}
          APPLE_API_KEY: \${{ secrets.APPLE_API_KEY }}
          APPLE_API_ISSUER: \${{ secrets.APPLE_API_ISSUER }}
      - name: Finalize macOS signing stage summary
        if: \${{ always() }}
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
          RIGHT_GIT_STAGE_ACTION: finalize
          RIGHT_GIT_STAGE_ROOT: \${{ runner.temp }}/right-git-stage-summary
          RIGHT_GIT_STAGE_EVIDENCE_ROOT: \${{ runner.temp }}/finalized-macos
          RIGHT_GIT_STAGE: macos-sign
          RIGHT_GIT_STAGE_PRODUCER: macos-sign-\${{ matrix.architecture }}
          RIGHT_GIT_STAGE_STATUS: \${{ job.status == 'success' && 'SUCCEEDED' || 'FAILED' }}
          RIGHT_GIT_STAGE_EXIT_CODE: \${{ job.status == 'success' && '0' || '1' }}
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          RIGHT_GIT_RUN_ID: \${{ github.run_id }}
          RIGHT_GIT_RUN_ATTEMPT: \${{ github.run_attempt }}
      - name: Upload signed & notarized macOS candidate
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-signed-macos-\${{ matrix.architecture }}-\${{ needs.admission.outputs.version }}-\${{ needs.admission.outputs.source_revision }}\${{ needs.admission.outputs.artifact_suffix }}
          path: |
${uploadPath}
          if-no-files-found: error
      - name: Upload macOS signing stage summary
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-macos-sign-\${{ matrix.architecture }}-\${{ needs.admission.outputs.version }}-\${{ needs.admission.outputs.source_revision }}\${{ needs.admission.outputs.artifact_suffix }}
          path: \${{ runner.temp }}/right-git-stage-summary/stage-summary.json
          if-no-files-found: error
      - name: Remove Apple signing material
        if: \${{ always() }}
        shell: bash
        run: |
          security delete-keychain "$RUNNER_TEMP/apple-signing.keychain-db" 2>/dev/null || true
          rm -f "$RUNNER_TEMP/apple-signing.p12" "$RUNNER_TEMP/AuthKey.p8"
`.trimEnd();
}

function releaseChainJobs(manifest, pm, artifactPrefix) {
  const chain = manifest.candidate?.releaseChain;
  if (!chain) return "";
  const windowsTarget = candidateTargets(manifest.candidate).find((target) => target.platform === "windows");
  const windowsRunner = windowsTarget.os;
  const windowsArchitecture = windowsTarget.architecture;
  const macosTarget = candidateTargets(manifest.candidate).find((target) => target.platform === "macos");
  const signMacos = manifest.candidate.signMacos === true;
  const macosDependency = signMacos ? ", macos-sign" : "";
  const macosSuccess = signMacos ? " && needs.macos-sign.result == 'success'" : "";
  // Run- and identity-bound: matches exactly what windows-sign/macos-sign
  // uploaded (see windowsSigningJob/macosSigningJob) — no matrix context is
  // available in these non-matrixed jobs, so the platform architecture is a
  // literal derived from the manifest's single windows/macos target.
  const VERSION = "${{ needs.admission.outputs.version }}";
  // Identity-bound artifact names end with the admission artifact_suffix (`-dry-run` on a rehearsal, empty otherwise).
  const SOURCE_REVISION = "${{ needs.admission.outputs.source_revision }}${{ needs.admission.outputs.artifact_suffix }}";
  const windowsSignedArtifact = `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-signed-windows-${windowsArchitecture}-${VERSION}-${SOURCE_REVISION}`;
  const windowsSummaryArtifact = `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-windows-sign-${windowsArchitecture}-${VERSION}-${SOURCE_REVISION}`;
  const macosSignedArtifact = signMacos ? `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-signed-macos-${macosTarget.architecture}-${VERSION}-${SOURCE_REVISION}` : "";
  const macosSummaryArtifact = signMacos ? `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-macos-sign-${macosTarget.architecture}-${VERSION}-${SOURCE_REVISION}` : "";
  // Candidate stage summaries are per (platform, architecture) — see the
  // `Upload candidate stage summary` step in the template. releaseChain
  // requires at most one target per signing platform (validateManifest), so
  // exactly one candidate-windows summary and at most one candidate-macos
  // summary exist for this manifest.
  const candidateWindowsSummaryArtifact = `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-candidate-windows-${windowsArchitecture}-${VERSION}-${SOURCE_REVISION}`;
  const candidateMacosSummaryArtifact = signMacos ? `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-candidate-macos-${macosTarget.architecture}-${VERSION}-${SOURCE_REVISION}` : "";
  const qualificationArtifact = `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-installed-qualification-${VERSION}-${SOURCE_REVISION}`;
  const qualificationSummaryArtifact = `${artifactPrefix}-\${{ github.run_id }}-\${{ github.run_attempt }}-summary-installed-qualification-${VERSION}-${SOURCE_REVISION}`;
  const macosDownload = signMacos
    ? `      - name: Download same-run signed & notarized macOS release
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${macosSignedArtifact}
          path: \${{ runner.temp }}/right-git-release/macos`
    : "";
  const macosSummaryDownload = signMacos
    ? `      - name: Download same-run macOS signing stage summary
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${macosSummaryArtifact}
          path: \${{ runner.temp }}/right-git-release/stage-summaries/macos-sign`
    : "";
  const candidateMacosSummaryDownload = signMacos
    ? `      - name: Download same-run candidate (macOS) stage summary
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${candidateMacosSummaryArtifact}
          path: \${{ runner.temp }}/right-git-release/stage-summaries/candidate-macos`
    : "";
  const finalizedMacosEnv = signMacos ? `\n          RIGHT_GIT_FINALIZED_MACOS_ROOT: \${{ runner.temp }}/right-git-release/macos` : "";
  // Evidence must be bound to this exact run — not merely this
  // version+source_revision — so a stale artifact from a different run
  // cannot satisfy verification. Every job/step from installed-qualification
  // onward carries RIGHT_GIT_RUN_ID/RIGHT_GIT_RUN_ATTEMPT alongside version
  // and source_revision.
  const runIdentityEnv = `          RIGHT_GIT_RUN_ID: \${{ github.run_id }}
          RIGHT_GIT_RUN_ATTEMPT: \${{ github.run_attempt }}`;
  const publishSecretEnvironment = (chain.publishSecrets ?? [])
    .map((name) => `          ${name}: \${{ secrets.${name} }}`)
    .join("\n");
  return `
  installed-qualification:
    needs: [admission, candidate, windows-sign]
    if: \${{ always() && needs.admission.outputs.signed_qualification == 'true' && needs.candidate.result == 'success' }}
    environment: ${releaseEnvironment(manifest, "qualification")}
    runs-on: \${{ vars.${chain.qualificationRunnerVariable} || '${windowsRunner}' }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: \${{ needs.admission.outputs.source_revision }}
          fetch-depth: 0
          persist-credentials: false
      - name: Fail closed when protected finalization is absent
        if: \${{ needs.windows-sign.result != 'success' }}
        shell: pwsh
        run: |
          throw 'installed_qualification_failed { reason: prerequisites_absent, detail: "same-run Windows signing/finalization did not succeed" }'
      - name: Download same-run finalized Windows release
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${windowsSignedArtifact}
          path: \${{ runner.temp }}/right-git-release/windows
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${String(manifest.matrix.node[0])}
          package-manager-cache: false
${pm.setup}
      - run: ${pm.installCommand}
      - name: Initialize installed-qualification stage summary
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
          RIGHT_GIT_STAGE_ACTION: init
          RIGHT_GIT_STAGE_ROOT: \${{ runner.temp }}/right-git-stage-summary
          RIGHT_GIT_STAGE_EVIDENCE_ROOT: \${{ runner.temp }}/right-git-release/qualification
          RIGHT_GIT_STAGE: installed-qualification
          RIGHT_GIT_STAGE_PRODUCER: installed-qualification
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
${runIdentityEnv}
      - name: Run installed-artifact qualification
        run: ${pm.runPrefix} ${chain.installedQualificationScript}
        env:
          GH_TOKEN: \${{ github.token }}
          RIGHT_GIT_FINALIZED_WINDOWS_ROOT: \${{ runner.temp }}/right-git-release/windows
          RIGHT_GIT_QUALIFICATION_EVIDENCE_ROOT: \${{ runner.temp }}/right-git-release/qualification
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
      - name: Finalize installed-qualification stage summary
        if: \${{ always() }}
        run: ${pm.runPrefix} ${manifest.candidate.stageSummaryScript}
        env:
          RIGHT_GIT_STAGE_ACTION: finalize
          RIGHT_GIT_STAGE_ROOT: \${{ runner.temp }}/right-git-stage-summary
          RIGHT_GIT_STAGE_EVIDENCE_ROOT: \${{ runner.temp }}/right-git-release/qualification
          RIGHT_GIT_STAGE: installed-qualification
          RIGHT_GIT_STAGE_PRODUCER: installed-qualification
          RIGHT_GIT_STAGE_STATUS: \${{ job.status == 'success' && 'SUCCEEDED' || 'FAILED' }}
          RIGHT_GIT_STAGE_EXIT_CODE: \${{ job.status == 'success' && '0' || '1' }}
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
${runIdentityEnv}
      - name: Upload installed qualification evidence
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${qualificationArtifact}
          path: \${{ runner.temp }}/right-git-release/qualification/**
          if-no-files-found: error
      - name: Upload installed-qualification stage summary
        if: \${{ always() }}
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${qualificationSummaryArtifact}
          path: \${{ runner.temp }}/right-git-stage-summary/stage-summary.json
          if-no-files-found: error
  publish:
    needs: [admission, installed-qualification${macosDependency}]
    if: \${{ needs.admission.outputs.publish == 'true' && needs.installed-qualification.result == 'success'${macosSuccess} }}
    environment: ${releaseEnvironment(manifest, "publication")}
    runs-on: \${{ vars.${chain.qualificationRunnerVariable} || '${windowsRunner}' }}
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: \${{ needs.admission.outputs.source_revision }}
          fetch-depth: 0
          persist-credentials: false
      - name: Download same-run finalized Windows release
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${windowsSignedArtifact}
          path: \${{ runner.temp }}/right-git-release/windows
${macosDownload}
      - name: Download same-run installed qualification evidence
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${qualificationArtifact}
          path: \${{ runner.temp }}/right-git-release/qualification
      - name: Download same-run candidate (Windows) stage summary
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${candidateWindowsSummaryArtifact}
          path: \${{ runner.temp }}/right-git-release/stage-summaries/candidate-windows
${candidateMacosSummaryDownload}
      - name: Download same-run Windows signing stage summary
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${windowsSummaryArtifact}
          path: \${{ runner.temp }}/right-git-release/stage-summaries/windows-sign
${macosSummaryDownload}
      - name: Download same-run installed-qualification stage summary
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${qualificationSummaryArtifact}
          path: \${{ runner.temp }}/right-git-release/stage-summaries/installed-qualification
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${String(manifest.matrix.node[0])}
          package-manager-cache: false
${pm.setup}
      - run: ${pm.installCommand}
      - name: Verify aggregated release evidence
        run: ${pm.runPrefix} ${chain.evidenceVerificationScript}
        env:
          GH_TOKEN: \${{ github.token }}
          RIGHT_GIT_FINALIZED_WINDOWS_ROOT: \${{ runner.temp }}/right-git-release/windows${finalizedMacosEnv}
          RIGHT_GIT_QUALIFICATION_EVIDENCE_ROOT: \${{ runner.temp }}/right-git-release/qualification
          RIGHT_GIT_STAGE_SUMMARY_ROOT: \${{ runner.temp }}/right-git-release/stage-summaries
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
${runIdentityEnv}
      - name: Publish qualified signed release
        run: ${pm.runPrefix} ${chain.publishScript}
        env:
          GH_TOKEN: \${{ github.token }}
          RIGHT_GIT_FINALIZED_WINDOWS_ROOT: \${{ runner.temp }}/right-git-release/windows${finalizedMacosEnv}
          RIGHT_GIT_QUALIFICATION_EVIDENCE_ROOT: \${{ runner.temp }}/right-git-release/qualification
          RIGHT_GIT_STAGE_SUMMARY_ROOT: \${{ runner.temp }}/right-git-release/stage-summaries
          RIGHT_GIT_RELEASE_VERSION: \${{ needs.admission.outputs.version }}
          RIGHT_GIT_SOURCE_REVISION: \${{ needs.admission.outputs.source_revision }}
${runIdentityEnv}
${publishSecretEnvironment}
`.trimEnd();
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
/** Every managed public workflow runs ownership scanning; repository metadata cannot opt it out. */
export function ownershipStep(manifest, lane = "ci") {
  const { name } = parsePackageManager(manifest.packageManager);
  const command = name === "pnpm" ? "pnpm exec right-release ownership --app ."
    : name === "npm" ? "npm exec -- right-release ownership --app ."
      : "yarn exec right-release ownership --app .";
  // Every lane stays advisory until ownership approval keys are enrolled (Adrian, 2026-10-09);
  // once enrolled, the flag is inert and findings block again.
  const ownershipCommand = `${command} --advisory-when-pending`;
  // Only release-candidate dispatches declare source_revision. Referencing
  // inputs.source_revision from other lanes makes actionlint reject their
  // generated workflows, even when guarded by an event-name expression.
  // rust-hybrid release-candidate is dispatch-only. The node release-candidate
  // also runs on a v* tag push, which has no input: it compares against the
  // pushed commit instead.
  const sourceRevision = lane !== "release-candidate"
    ? "\${{ github.sha }}"
    : manifest.profile === "rust-hybrid"
      ? "\${{ inputs.source_revision }}"
      : "\${{ inputs.source_revision || github.sha }}";
  // Ownership approval supports only macOS and Windows (Orthic-Labs/rightkit#245). A rust-hybrid ci
  // job that runs every matrix.os runner (ciAllOs) and declares a Linux runner skips the step there.
  // Manifests with no Linux runner in that matrix render the step unchanged.
  const skipOnLinux = lane === "ci" && manifest.profile === "rust-hybrid" && manifest.ciAllOs === true
    && manifest.matrix.os.some((runner) => /^(ubuntu|linux)/i.test(runner));
  const linuxGuard = skipOnLinux ? "        if: ${{ runner.os != 'Linux' }}\n" : "";
  return `\n      - name: RightKit ownership (protected registry roots)\n${linuxGuard}        shell: bash\n        run: test "$(git rev-parse HEAD)" = "${sourceRevision}" && ${ownershipCommand}`;
}

/**
 * pnpm stale-lockfile hint for the ci lane. The step runs only when the step
 * before it (the frozen install) failed. It regenerates the lockfile with
 * `pnpm install --lockfile-only` and prints the lockfile's git diff as a patch
 * block. It never changes the job result, which the failed install already
 * decided. Returns "" for npm and yarn, so their renders stay unchanged.
 */
export function lockfilePatchStep(manifest) {
  const { name } = parsePackageManager(manifest.packageManager);
  if (name !== "pnpm") return "";
  return [
    "",
    "      - name: Print pnpm-lock.yaml patch when the lockfile is stale",
    "        if: failure()",
    "        shell: bash",
    "        run: |",
    "          if pnpm install --lockfile-only; then",
    "            if git diff --quiet -- pnpm-lock.yaml; then",
    '              echo "::notice::pnpm-lock.yaml already matches package.json; the frozen install failed for another reason."',
    "            else",
    '              echo "::group::pnpm-lock.yaml patch (apply and commit)"',
    "              git diff -- pnpm-lock.yaml",
    '              echo "::endgroup::"',
    '              echo "::error file=pnpm-lock.yaml::pnpm-lock.yaml is stale; commit the patch above."',
    "            fi",
    "          else",
    '            echo "::error::pnpm install --lockfile-only failed; no lockfile patch was produced."',
    "          fi",
  ].join("\n");
}

// Action pins of the check job follow the template it is rendered into, so one workflow never mixes two pin styles:
// the node-profile lane templates pin by tag, swift-check and the rust-hybrid templates by commit.
const CHECK_SETUP_NODE_SHA_PIN = "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0";
const CHECK_ACTION_PINS = Object.freeze({
  tag: Object.freeze({ checkout: "actions/checkout@v5", setupNode: "actions/setup-node@v5" }),
  swift: Object.freeze({ checkout: "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2", setupNode: CHECK_SETUP_NODE_SHA_PIN }),
  sha: Object.freeze({ checkout: "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1", setupNode: CHECK_SETUP_NODE_SHA_PIN }),
});
// Lanes whose jobs compile or test the repository and therefore sit behind the check job (rightkit#833, pipeline invariant P1).
// `publish-npm` is one of them: it checks out the release tag, installs, rebuilds the tarball from source (build-candidate.mjs),
// runs check-release.mjs and only then publishes, so the check runs on the tag's commit before any of that. The others render
// without it: `regenerate` exists to repair what the check flags (derived files, lockfiles), `sign-existing` and `supersede`
// are admitted by @rightkit/release only in their exact rendered shape (`needs: [private-repo-guard]`), and
// `publish-package-managers` builds and tests nothing (it installs, syntax-checks the committed formula and opens a tap pull
// request, or submits a WinGet manifest, for a version that is already released).
export const CHECK_JOB_LANES = Object.freeze(["ci", "qualification", "package-smoke", "release-candidate", "swift-check", "publish-npm"]);

/**
 * The RightKit check job: the first job of every lane that compiles or tests, so nothing builds or qualifies a tree the
 * check refused. It needs only the private-repo guard; every other job of the workflow needs it (directly in `needs`, or
 * through a job that does). It installs the repository's own dependencies without running their scripts, then runs
 * `rightkit-gate check --commit HEAD` from the installed @rightkit/release (committed files only: no network, compile or
 * broker), so the repository must depend on a @rightkit/release that has the command. The install is not filtered to
 * @rightkit/release: `pnpm install --filter` selects workspace projects, and in a consumer @rightkit/release is a registry
 * dependency, so the filter matches nothing, installs nothing and exits 0. npm and yarn call the bin by its path because
 * `npm exec` fetches an unknown name from the registry in CI. A release-candidate checks the revision it builds, and
 * publish-npm checks the release tag it builds and publishes. Returns "" for lanes outside CHECK_JOB_LANES; otherwise the
 * job text, ending with a newline.
 */
export function checkJob(manifest, lane = "ci") {
  if (!CHECK_JOB_LANES.includes(lane)) return "";
  const pm = parsePackageManager(manifest.packageManager);
  const rustHybrid = manifest.profile === "rust-hybrid";
  const pins = rustHybrid ? CHECK_ACTION_PINS.sha : lane === "swift-check" ? CHECK_ACTION_PINS.swift : CHECK_ACTION_PINS.tag;
  // The runner of the template's own guard job: the check reads files and needs no platform.
  const runner = rustHybrid || lane === "swift-check" ? "ubuntu-24.04" : "ubuntu-latest";
  // The revision the lane builds: a release-candidate's dispatched (or pushed) commit, the release tag publish-npm publishes.
  const ref = lane === "publish-npm" ? "${{ github.event.release.tag_name }}"
    : lane !== "release-candidate" ? null
      : rustHybrid ? "${{ inputs.source_revision }}" : "${{ inputs.source_revision || github.sha }}";
  // The same frozen install every other job runs (pm.installCommand), without lifecycle scripts; `--ignore-scripts` is a flag of
  // `pnpm install`, `npm ci` and `yarn install` alike. Only the gate invocation differs per manager.
  const install = `${pm.installCommand} --ignore-scripts`;
  const gate = { pnpm: "pnpm exec rightkit-gate", npm: "./node_modules/.bin/rightkit-gate", yarn: "./node_modules/.bin/rightkit-gate" }[pm.name];
  // The ci job prints the stale-lockfile patch after a failed frozen install; with check first it would never run, so check prints it.
  const lockfilePatch = lane === "ci" ? lockfilePatchStep(manifest) : "";
  return [
    "  check:",
    "    # RightKit check: committed files only, no compile; every finding in one run. Every other job needs it.",
    "    needs: [private-repo-guard]",
    `    runs-on: ${runner}`,
    "    timeout-minutes: 15",
    "    steps:",
    `      - uses: ${pins.checkout}`,
    "        with:",
    ...(ref ? [`          ref: ${ref}`] : []),
    "          fetch-depth: 1",
    "          persist-credentials: false",
    `      - uses: ${pins.setupNode}`,
    "        with:",
    `          node-version: ${JSON.stringify(String(manifest.matrix.node[0]))}`,
    "          package-manager-cache: false",
    ...(pm.setup ? [pm.setup] : []),
    `      - run: ${install}`,
    ...(lockfilePatch ? lockfilePatch.slice(1).split("\n") : []),
    `      - run: ${gate} check --commit HEAD`,
    "",
  ].join("\n");
}

/**
 * swift.privatePackages: configure step, placed directly before the first step
 * in each opted-in job that builds or tests project code (the ci job, the
 * qualification job, the release-candidate candidate job, the regenerate job and
 * the parallel dev-artifact-mac job). It refuses to overwrite an
 * existing ~/.netrc, writes one machine entry per private host with mode 600,
 * and adds a job-scoped git insteadOf rewrite. The token reaches the step only
 * through env, never through the run script text. Returns "" when unset; the
 * value starts with a newline and has no trailing newline.
 */
export function swiftPrivatePackagesConfigureStep(manifest) {
  const entries = manifest.swift?.privatePackages;
  if (!Array.isArray(entries) || entries.length === 0) return "";
  const env = entries.map((entry, index) => `          RIGHT_GIT_PRIVATE_TOKEN_${index}: \${{ secrets.${entry.secret} }}`);
  const checks = entries.map((entry, index) => `          test -n "$RIGHT_GIT_PRIVATE_TOKEN_${index}" || { echo "::error::right-git: secret ${entry.secret} is unavailable to this run"; exit 1; }`);
  const netrc = entries.map((entry, index) => `          printf 'machine %s\\nlogin x-access-token\\npassword %s\\n' '${entry.host}' "$RIGHT_GIT_PRIVATE_TOKEN_${index}" >> "$HOME/.netrc"`);
  const insteadOf = entries.map((entry, index) => `          git config --global "url.https://x-access-token:$RIGHT_GIT_PRIVATE_TOKEN_${index}@${entry.host}/.insteadOf" "https://${entry.host}/"`);
  return [
    "",
    "      - name: Configure private Swift package access",
    "        shell: bash",
    "        env:",
    ...env,
    "        run: |",
    "          set -euo pipefail",
    "          umask 077",
    ...checks,
    "          test ! -e \"$HOME/.netrc\" || { echo \"::error::right-git: $HOME/.netrc already exists on this runner; refusing to overwrite it\"; exit 1; }",
    "          : > \"$HOME/.netrc\"",
    // The marker follows creation so cleanup removes this file even if a later line fails.
    "          : > \"$RUNNER_TEMP/right-git-private-netrc\"",
    ...netrc,
    "          chmod 600 \"$HOME/.netrc\"",
    ...insteadOf,
  ].join("\n");
}

/**
 * swift.privatePackages: cleanup step, rendered after the last step that builds
 * or tests project code in the same job (the end of the ci job; before any
 * push or upload elsewhere) and run even after a failed build. It only removes
 * what the configure step created (the marker file), so a cancelled run never
 * deletes a pre-existing ~/.netrc. Returns "" when unset; same newline convention as the configure step.
 */
export function swiftPrivatePackagesCleanupStep(manifest) {
  const entries = manifest.swift?.privatePackages;
  if (!Array.isArray(entries) || entries.length === 0) return "";
  const env = entries.map((entry, index) => `          RIGHT_GIT_PRIVATE_TOKEN_${index}: \${{ secrets.${entry.secret} }}`);
  const removals = entries.map((entry, index) => `            git config --global --remove-section "url.https://x-access-token:$RIGHT_GIT_PRIVATE_TOKEN_${index}@${entry.host}/" || true`);
  return [
    "",
    "      - name: Remove private Swift package access",
    "        if: ${{ always() }}",
    "        shell: bash",
    "        env:",
    ...env,
    "        run: |",
    "          if [ -f \"$RUNNER_TEMP/right-git-private-netrc\" ]; then",
    "            rm -f \"$HOME/.netrc\"",
    ...removals,
    "            rm -f \"$RUNNER_TEMP/right-git-private-netrc\"",
    "          fi",
  ].join("\n");
}

/**
 * gate.shards / gate.quick render values. Without shards the values are the text the template held before the
 * placeholders existed, plus the `check` job every ci-lane job now needs (see checkJob).
 */
function gateShardContext(manifest, pm, lane) {
  const shardIds = gateShardIds(manifest);
  if (lane !== "ci" || shardIds.length === 0) {
    return { CI_NEEDS: "[private-repo-guard, check]", CI_SHARD_KEY: "", GATE_ARGS: "", CI_QUICK_JOB: "", CI_SUMMARY_JOB: "" };
  }
  const quick = manifest.gate.quick === true;
  const quickJob = quick ? [
    "  quick:",
    "    # gate.quick: the fast deterministic checks, before any gate shard starts.",
    "    needs: [private-repo-guard, check]",
    "    runs-on: ubuntu-24.04",
    "    timeout-minutes: 20",
    "    steps:",
    "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1",
    "        with:",
    "          fetch-depth: 0",
    "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0",
    "        with:",
    `          node-version: ${JSON.stringify(String(manifest.matrix.node[0]))}`,
    "          package-manager-cache: false",
    ...(pm.setup ? [pm.setup] : []),
    `      - run: ${pm.installCommand}`,
    "      - uses: dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0",
    "        with:",
    "          components: rustfmt, clippy",
    `      - run: bash ./${manifest.gate.path} --quick`,
    "        env:",
    "          RIGHT_GIT_GATE_QUICK: \"true\"",
    "",
  ].join("\n") : "";
  const needs = quick ? "[ci, quick]" : "[ci]";
  const summaryJob = [
    "",
    "  ci-summary:",
    "    # gate.shards: passes only when every shard passed on every runner" + (quick ? " and the quick checks passed." : "."),
    `    needs: ${needs}`,
    "    if: ${{ always() }}",
    "    runs-on: ubuntu-24.04",
    "    timeout-minutes: 5",
    "    steps:",
    "      - name: Every gate shard passed on every runner",
    "        env:",
    "          CI_RESULT: ${{ needs.ci.result }}",
    ...(quick ? ["          QUICK_RESULT: ${{ needs.quick.result }}"] : []),
    "        run: |",
    "          set -euo pipefail",
    "          test \"$CI_RESULT\" = success",
    ...(quick ? ["          test \"$QUICK_RESULT\" = success"] : []),
    "",
  ].join("\n");
  return {
    CI_NEEDS: quick ? "[private-repo-guard, check, quick]" : "[private-repo-guard, check]",
    CI_SHARD_KEY: "-${{ matrix.shard }}",
    GATE_ARGS: " --group ${{ matrix.shard }}",
    CI_QUICK_JOB: quickJob,
    CI_SUMMARY_JOB: summaryJob,
  };
}

/**
 * Optional QA evidence upload, rendered after the rust-hybrid gate step.
 * Returns "" when qaEvidencePath is unset, so the rendered workflow stays
 * byte-identical to a manifest without the key.
 */
export function qaEvidenceStep(manifest) {
  if (manifest.qaEvidencePath === undefined) return "";
  const retentionDays = manifest.qaEvidenceRetentionDays ?? QA_EVIDENCE_DEFAULT_RETENTION_DAYS;
  return [
    "      - name: Upload QA evidence",
    "        if: always()",
    `        uses: ${UPLOAD_ARTIFACT_PIN}`,
    "        with:",
    `          name: qa-evidence-\${{ matrix.os || runner.os }}${gateShardIds(manifest).length > 0 ? "-${{ matrix.shard }}" : ""}-\${{ github.run_attempt }}`,
    `          path: \${{ runner.temp }}/${manifest.qaEvidencePath}`,
    `          retention-days: ${retentionDays}`,
    "          if-no-files-found: ignore",
    "",
  ].join("\n");
}

export function buildRenderContext(manifest, repoPath, lane = "ci") {
  const pm = parsePackageManager(manifest.packageManager);
  const projectName = deriveProjectName(repoPath);
  const artifactPrefix = projectName.toLowerCase().replace(/[^a-z0-9-]+/g, "-");
  const rustWorkspaces = manifest.rustCache?.workspaces ?? ["engine"];
  const configuredWindowsRunner = manifest.matrix.os.find((runner) => runner.startsWith("windows-"));
  const ciOs = configuredWindowsRunner ?? manifest.matrix.os[0];
  const ciMatrixOs = manifest.ciAllOs === true
    ? [
      "        # ciAllOs: the gate runs on every matrix.os runner, in declared order.",
      `        os: ${yamlFlowStringList(manifest.matrix.os)}`,
    ]
    : [
      "        # Full validation stays on one declared runner; prefer configured",
      "        # Windows when available, otherwise preserve first configured OS.",
      `        os: ${yamlFlowStringList([ciOs])}`,
    ];

  const shardIds = gateShardIds(manifest);
  if (shardIds.length > 0) ciMatrixOs.push(`        shard: ${yamlFlowStringList(shardIds)}`);
  const cacheKeyLine = rustCacheKeyLine(manifest, lane);
  const context = {
    OS_MATRIX: yamlFlowStringList(manifest.matrix.os),
    CI_MATRIX_OS: ciMatrixOs.join("\n"),
    NODE_MATRIX: yamlFlowStringList(manifest.matrix.node),
    NODE_PUBLISH_VERSION: JSON.stringify(String(manifest.matrix.node[0])),
    PM_NAME: pm.name,
    PM_VERSION: pm.version,
    PM_CACHE: pm.name,
    INSTALL_COMMAND: pm.installCommand,
    PM_RUN: pm.runPrefix,
    PACKAGE_MANAGER_SETUP: pm.setup,
    ARTIFACT_PREFIX: artifactPrefix,
    DEFAULT_BRANCH: manifest.defaultBranch ?? "main",
    TEMPLATE_VERSION: manifest.templateVersion,
    RUST_CACHE_WORKSPACES: rustWorkspaces.map((workspace) => `            ${workspace} -> target`).join("\n"),
    RUST_REQUIRED_FOR_GATE: manifest.rustCache?.requiredForGate === true ? "true" : "false",
    DEV_QUALIFICATION_JOB: devQualificationJob(manifest, pm, artifactPrefix, cacheKeyLine),
    WINDOWS_SIGNING_JOB: windowsSigningJob(manifest, pm, artifactPrefix),
    MACOS_SIGNING_JOB: macosSigningJob(manifest, pm, artifactPrefix),
    RELEASE_CHAIN_JOBS: releaseChainJobs(manifest, pm, artifactPrefix),
    OWNERSHIP_STEP: ownershipStep(manifest, lane),
    PNPM_LOCK_PATCH_STEP: lockfilePatchStep(manifest),
    SWIFT_PRIVATE_CONFIGURE_STEP: swiftPrivatePackagesConfigureStep(manifest),
    SWIFT_PRIVATE_CLEANUP_STEP: swiftPrivatePackagesCleanupStep(manifest),
    QA_EVIDENCE_STEP: qaEvidenceStep(manifest) + devArtifactCi(manifest, { lane, artifactPrefix }),
    CI_DISPATCH_TRIGGER: devArtifactDispatchTrigger(manifest),
    CI_CACHE_WORKSPACE_CRATES: manifest.ciIncremental === true ? "true" : "false",
    CI_CARGO_INCREMENTAL: manifest.ciIncremental === true ? "1" : "0",
    // sccache refuses CARGO_INCREMENTAL=1; incremental CI relies on the rust-cache target dir instead.
    CI_RUSTC_WRAPPER: manifest.ciIncremental === true ? '""' : "${{ steps.rust-changes.outputs.required == 'true' && 'sccache' || '' }}",
    CI_DERIVED_DATA_STEP: derivedDataStep(manifest),
    // Default-branch pushes are never cancelled by the workflow-level group: the `supersede` job decides
    // (queue mode: nobody cancels them). PR and dispatch runs keep cancelling superseded runs.
    CI_CANCEL_IN_PROGRESS: CI_CANCEL_MAIN_QUEUE_EXPRESSION,
    // swift-check has no supersede job: it keeps cancelling superseded runs unless queue mode is set.
    CI_SWIFT_CANCEL_IN_PROGRESS: manifest.concurrency?.mainPushes === "queue" ? CI_CANCEL_MAIN_QUEUE_EXPRESSION : "true",
    CI_CONCURRENCY_COMMENT: manifest.concurrency?.mainPushes === "queue"
      ? `# concurrency.mainPushes queue, recorded exception to one CI run per repo: ${manifest.concurrency.reason.trim()}\n`
      : "",
    CI_CONCURRENCY_GROUP: manifest.concurrency?.mainPushes === "queue" ? CI_GROUP_PER_REF : CI_GROUP_SUPERSEDE,
    CHECK_JOB: checkJob(manifest, lane),
    CI_SUPERSEDE_JOB: manifest.concurrency?.mainPushes === "queue" ? "" : supersedeJob(manifest),
    CI_JOB_CONCURRENCY: manifest.concurrency?.mainPushes === "queue" ? "" : ciJobConcurrency(manifest),
    ...gateShardContext(manifest, pm, lane),
    RUST_CACHE_KEY: cacheKeyLine ? `${cacheKeyLine}\n` : "",
  };

  // Every key below is populated ONLY when its source manifest section is
  // actually present — mirroring the publish.npm/homebrew/winget pattern.
  // This keeps the fail-closed check just below honest: a key that is
  // legitimately inapplicable to this manifest (e.g. CANDIDATE_* for a
  // non-rust-hybrid manifest, GATE_PATH for a non-rust-hybrid profile) is
  // simply ABSENT from the context rather than present-with-undefined, so
  // render.mjs's own `Object.hasOwn` check does the right thing if a
  // template ever references a token this manifest didn't opt into.
  if (manifest.swift?.check) {
    const check = manifest.swift.check;
    Object.assign(context, {
      SWIFT_CHECK_RUNNER: check.runner,
      SWIFT_CHECK_XCODE_VERSION: check.xcodeVersion,
      SWIFT_CHECK_DIRECTORY: check.projectDirectory,
      SWIFT_CHECK_SPEC: check.spec,
      SWIFT_CHECK_PROJECT: check.project,
      SWIFT_CHECK_SCHEME: check.scheme,
      SWIFT_CHECK_XCODEGEN_VERSION: check.xcodegenVersion,
    });
  }
  if (manifest.gate) {
    context.GATE_PATH = manifest.gate.path;
  }
  if (manifest.regenerate) {
    context.REGENERATE_SCRIPT = manifest.regenerate.script;
  }
  if (manifest.signExisting && lane === "sign-existing") {
    context.SIGN_EXISTING_JOB = signExistingJob(manifest);
  }
  if (manifest.candidate) {
    context.CANDIDATE_BUILD_SCRIPT = manifest.candidate.buildScript;
    context.CANDIDATE_CHECK_SCRIPT = manifest.candidate.checkScript;
    context.CANDIDATE_ARTIFACT_ROOT = manifest.candidate.artifactRoot;
    context.CANDIDATE_OS_MATRIX = yamlFlowStringList(manifest.candidate.os ?? manifest.matrix.os);
    context.CANDIDATE_MATRIX = candidateMatrix(manifest);
    // admissionScript & stageSummaryScript are required candidate fields
    // (see validateManifest) whenever release-candidate is configured at
    // all — the admission job and the candidate job's stage-summary steps
    // are unconditional parts of the rendered template, independent of
    // signing/releaseChain.
    context.RELEASE_ADMISSION_SCRIPT = manifest.candidate.admissionScript;
    context.RELEASE_STAGE_SUMMARY_SCRIPT = manifest.candidate.stageSummaryScript;
  }

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

  // F-undefined-script fix: render.mjs's `Object.hasOwn` check only proves a
  // key EXISTS in the context — it does not prove the key resolved to a real
  // value. Every field above that is sourced from an optional manifest path
  // (candidate.*, candidate.releaseChain.* via the job-builder functions,
  // gate.path) must resolve to a real string once its guarding `if` above
  // decided the section applies; a bug in one of those guards (or a
  // mis-nested manifest field) must fail the render loudly here rather than
  // silently emit a literal "undefined" into a workflow file that GitHub
  // Actions will then try to execute as a real step (e.g. `run: pnpm run
  // undefined`).
  for (const [key, value] of Object.entries(context)) {
    if (value === undefined || value === null) {
      throw new Error(`buildRenderContext: placeholder \${${key}} resolved to ${value} — check the manifest's candidate/releaseChain/gate/publish configuration`);
    }
  }

  return context;
}
