// First-time setup: write `.rightgit.json` into a target repo and render its
// workflows (DESIGN.md §4). Refuses private/internal/unknown repos (I1) and
// refuses to clobber an already-installed repo — that's what `sync` is for.
//
// Matrix/packageManager/templateVersion defaults are the exact example
// values DESIGN.md §4 documents for `.rightgit.json`, not invented policy.

import fs from "node:fs";
import path from "node:path";
import { assertPublic } from "./policy/visibility-gate.mjs";
import { MANIFEST_FILENAME, validateManifest } from "./manifest.mjs";
import { applyPlan, planSync } from "./sync.mjs";

export const DEFAULT_MATRIX = { os: ["ubuntu-latest", "macos-latest", "windows-latest"], node: ["22.13", "24"] };
export const DEFAULT_PACKAGE_MANAGER = "pnpm@11.24.0";
export const DEFAULT_TEMPLATE_VERSION = "1.7.0";

/**
 * opts: { lanes: string[], matrix?, packageManager?, templateVersion?,
 *         provenance?: boolean, homebrewTap?, homebrewFormula?, wingetId? }
 * Never throws. Returns a typed result: { ok, stage, errors, manifestPath?, plan? }
 */
export async function installCommand(repoPath, opts = {}, deps = {}) {
  const visibility = await assertPublic(repoPath, deps);
  if (!visibility.allowed) {
    return { ok: false, stage: "visibility", errors: [visibility.reason] };
  }

  const manifestPath = path.join(repoPath, MANIFEST_FILENAME);
  if (fs.existsSync(manifestPath)) {
    return { ok: false, stage: "exists", errors: [`${manifestPath} already exists; use 'right-git sync' to update an installed repo`] };
  }

  const lanes = opts.lanes;
  if (!Array.isArray(lanes) || lanes.length === 0) {
    return { ok: false, stage: "lanes", errors: ["--lanes is required, e.g. --lanes ci,qualification,package-smoke"] };
  }

  const manifest = {
    schemaVersion: 1,
    compile: "github-actions-only",
    lanes,
    matrix: opts.matrix || DEFAULT_MATRIX,
    packageManager: opts.packageManager || DEFAULT_PACKAGE_MANAGER,
    templateVersion: opts.templateVersion || DEFAULT_TEMPLATE_VERSION,
  };
  if (opts.profile) manifest.profile = opts.profile;
  if (opts.gatePath) manifest.gate = { path: opts.gatePath };

  if (lanes.includes("publish-npm")) {
    manifest.publish = { ...manifest.publish, npm: { oidc: true, provenance: opts.provenance !== false } };
  }
  if (lanes.includes("publish-package-managers")) {
    if (!opts.homebrewTap || !opts.homebrewFormula || !opts.wingetId) {
      return {
        ok: false,
        stage: "publish-package-managers-config",
        errors: ["lane 'publish-package-managers' requires --homebrew-tap, --homebrew-formula, and --winget-id"],
      };
    }
    manifest.publish = {
      ...manifest.publish,
      homebrew: { tap: opts.homebrewTap, formulaName: opts.homebrewFormula },
      winget: { packageIdentifier: opts.wingetId },
    };
  }

  const { valid, errors } = validateManifest(manifest);
  if (!valid) return { ok: false, stage: "manifest-invalid", errors };

  const manifestBytes = `${JSON.stringify(manifest, null, 2)}\n`;
  const plan = await planSync(repoPath, deps, manifest);
  if (!plan.ok) return { ok: false, stage: plan.stage, errors: plan.errors, plan };
  fs.writeFileSync(manifestPath, manifestBytes, "utf8");
  const applied = applyPlan(repoPath, plan);
  if (!applied.ok) {
    if (fs.readFileSync(manifestPath, "utf8") === manifestBytes) fs.rmSync(manifestPath);
    return { ok: false, stage: applied.stage, errors: applied.errors, plan };
  }
  return { ok: true, stage: "synced", errors: [], manifestPath, plan };
}
