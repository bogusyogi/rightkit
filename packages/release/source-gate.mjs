// Release source gate.
//
// A build or upload may only run from a clean checkout, so every sealed artifact
// binds to an exact commit that exists in history rather than to whatever happened
// to be in the working tree. Upload separately re-checks that the sealed manifest's
// commit still matches HEAD, so the pair pins source identity end to end.
//
// This replaces the r7 verification-ticket / admin-broker layer removed in 578d7ac5.
// That layer defended against a hostile local admin, which is not a threat here; the
// real risk is shipping stale, dirty, or untested source. Distribution trust still
// comes from OS code signing, notarization, and the Tauri updater key — untouched.

export function assertCleanSource({ status, commandId = "release" }) {
  if (typeof status !== "string") throw new Error("release source status is required");
  if (status.length !== 0) {
    const files = status.split("\0").filter(Boolean).map((entry) => entry.slice(3)).slice(0, 10);
    throw new Error(`${commandId} requires a clean working tree; commit or stash first:\n${files.map((file) => `- ${file}`).join("\n")}`);
  }
}
