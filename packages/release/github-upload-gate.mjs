// Authorization shared by every command that can send a sealed installer release to GitHub Releases.
//
// `right-release upload` (alias `publish`) and `right-release github --release` both end in
// publishGitHubRelease(). Each calls authorizeGitHubUpload() before it plans or contacts GitHub, so the
// rules below exist once:
//   - an explicit tier, patch or update, never inferred;
//   - the primary checkout, a clean tree, and a sealed release built from the current HEAD;
//   - sealed ownership evidence that still matches the committed source;
//   - an explicit distribution.provider of github-releases and a distribution.repository, with any --repo
//     argument required to equal it (the origin remote is never consulted).
// Order matters: the release config is a module that runs on import, so it is loaded only after the tree is
// proven clean and the sealed commit equals HEAD.
import { existsSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { verifyOwnershipEvidence } from "./ownership-evidence.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { verifySealedRelease } from "./release-state.mjs";
import { assertCleanSource } from "./source-gate.mjs";

/** Throws unless the operator named a tier. There is no default and no inference from the version or the notes. */
export function assertUploadTier(tier) {
  if (tier !== "patch" && tier !== "update") throw new Error("tier is required (patch|update)");
  return tier;
}

/**
 * Returns the GitHub repository a release config publishes to, or throws.
 * `requestedRepo` is an optional command-line `--repo`; it may only restate the configured repository.
 */
export function resolveGitHubDistribution(releaseConfig, requestedRepo = "") {
  if (releaseConfig?.distribution?.provider !== "github-releases") {
    throw new Error("release config distribution.provider must be github-releases");
  }
  const configuredRepo = releaseConfig?.distribution?.repository ?? "";
  if (!configuredRepo) throw new Error("release config distribution.repository is required for github-releases");
  if (requestedRepo && requestedRepo !== configuredRepo) throw new Error(`--repo must match configured distribution.repository (${configuredRepo})`);
  return configuredRepo;
}

/**
 * Runs every local check that must pass before a sealed release may be planned for GitHub. Performs no network
 * call and writes nothing. Resolves with the verified inputs the caller needs; throws an Error on the first refusal.
 */
export async function authorizeGitHubUpload({
  command = "right-release upload",
  tier,
  releaseId,
  platform,
  configName = "right-release.config.mjs",
  repo = "",
  cwd = process.cwd(),
}) {
  assertUploadTier(tier);
  if (!releaseId) throw new Error("--release is required");
  if (platform !== "win" && platform !== "mac") throw new Error("--platform must be win or mac");

  const repoRoot = git(cwd, ["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot);
  const candidateCommit = git(repoRoot, ["rev-parse", "HEAD"]);
  assertCleanSource({
    status: git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
    commandId: command,
  });
  const platformDir = platform === "win" ? "windows" : "mac";
  const sealed = verifySealedRelease(path.join(repoRoot, ".right-release", "sealed", releaseId, platformDir));
  if (sealed.manifest.commit !== candidateCommit) throw new Error("sealed release was built from a different commit than the current checkout");

  const configPath = path.resolve(cwd, configName);
  if (!existsSync(configPath)) throw new Error(`release config is required: ${configName}`);
  const releaseConfig = (await import(`${pathToFileURL(configPath).href}?upload=${Date.now()}`)).default;
  const ownership = verifyOwnershipEvidence(sealed.manifest.ownershipEvidence, repoRoot, path.dirname(configPath));
  if (!ownership.ok) throw new Error(`sealed ownership evidence rejected: ${ownership.reason}`);
  const configuredRepo = resolveGitHubDistribution(releaseConfig, repo);

  return { repoRoot, platformDir, sealed, configPath, releaseConfig, repo: configuredRepo };
}

function git(cwd, runArgs) {
  const result = spawnSync("git", runArgs, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`git ${runArgs.join(" ")} failed: ${result.stderr}`);
  return result.stdout.trim();
}
