// Dependency-free refusal shared by every module that can put bytes in a registry. RightKit crates and @rightkit
// packages reach a registry from CI only (publish.yml qualification, the approval commit in ci/approved-artifacts.json,
// publish-reviewed.yml). No environment variable, flag or caller-set GITHUB_ACTIONS lifts this: CI uploads go through
// scripts/ci/publish.mjs and scripts/ci/reviewed-candidate.mjs, never through right-release.
export const HOST_UPLOAD_REFUSED = "RK_HOST_UPLOAD_REFUSED";
export function hostUploadRefusalMessage(what) {
  return `${what}: host uploads are retired; RightKit publishes only through CI's approved-hash path (publish.yml, approval in ci/approved-artifacts.json, publish-reviewed.yml). Use --dry-run, preflight or --local --qualify-only to qualify locally.`;
}
export function refuseHostUpload(what) {
  throw Object.assign(new Error(hostUploadRefusalMessage(what)), { code: HOST_UPLOAD_REFUSED });
}
