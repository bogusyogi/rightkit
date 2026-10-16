import { existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import path from "node:path";

export const GITHUB_ACTIONS_ONLY = "github-actions-only";
export const GITHUB_ACTIONS_ONLY_ERROR = "RK_GITHUB_ACTIONS_ONLY";

function directoryFrom(value) {
  const selected = path.resolve(value ?? process.cwd());
  try { return statSync(selected).isDirectory() ? realpathSync(selected) : path.dirname(realpathSync(selected)); }
  catch { return selected; }
}

function orthicRemote(root) {
  const config = path.join(root, ".git", "config");
  if (!existsSync(config)) return false;
  try { return /github\.com(?::|\/)Orthic-Labs\//i.test(readFileSync(config, "utf8")); }
  catch { return false; }
}

export function releaseExecutionPolicy(cwd = process.cwd()) {
  let cursor = directoryFrom(cwd);
  const filesystemRoot = path.parse(cursor).root;
  for (;;) {
    const manifestPath = path.join(cursor, ".rightgit.json");
    if (existsSync(manifestPath)) {
      let manifest;
      try { manifest = JSON.parse(readFileSync(manifestPath, "utf8")); }
      catch (error) { throw Object.assign(new Error(`invalid public repository execution policy: ${manifestPath}: ${error.message}`), { code: "RK_EXECUTION_POLICY_INVALID" }); }
      if (orthicRemote(cursor) && manifest.compile !== GITHUB_ACTIONS_ONLY) {
        throw Object.assign(new Error(`Orthic Labs public repository requires compile=${GITHUB_ACTIONS_ONLY}: ${manifestPath}`), { code: "RK_EXECUTION_POLICY_MISSING" });
      }
      return Object.freeze({ root: cursor, compile: manifest.compile ?? null });
    }
    if (orthicRemote(cursor)) throw Object.assign(new Error(`Orthic Labs public repository requires .rightgit.json with compile=${GITHUB_ACTIONS_ONLY}: ${cursor}`), { code: "RK_EXECUTION_POLICY_MISSING" });
    if (cursor === filesystemRoot) return Object.freeze({ root: null, compile: null });
    cursor = path.dirname(cursor);
  }
}

export const LOCAL_BUILD_FILE = ".rightkit-local-build.json";

// Adrian's local-build opt-in (2026-10-14): a committed .rightkit-local-build.json naming this exact
// repository allows local build, test and publish. Mirrors the build-control hook's check.
export function localBuildGranted(root) {
  if (!root) return false;
  try {
    const grant = JSON.parse(readFileSync(path.join(root, LOCAL_BUILD_FILE), "utf8"));
    if (grant?.allow !== "build-test-publish" || typeof grant.repository !== "string") return false;
    const config = readFileSync(path.join(root, ".git", "config"), "utf8");
    const origin = /\[remote "origin"\][^\[]*?url\s*=\s*(\S+)/i.exec(config)?.[1] ?? "";
    const repo = /github\.com[:/]([^/]+\/[^/.\s]+?)(?:\.git)?\/?$/i.exec(origin)?.[1];
    return !!repo && repo.toLowerCase() === grant.repository.toLowerCase();
  } catch { return false; }
}

export function assertReleaseExecutionAllowed({ cwd = process.cwd(), environment = process.env, command = "build" } = {}) {
  const policy = releaseExecutionPolicy(cwd);
  if (policy.compile === GITHUB_ACTIONS_ONLY && environment.GITHUB_ACTIONS !== "true" && !localBuildGranted(policy.root)) {
    throw Object.assign(new Error(`${command} is GitHub Actions only for ${policy.root}; push changes & use managed CI`), { code: GITHUB_ACTIONS_ONLY_ERROR });
  }
  return policy;
}
