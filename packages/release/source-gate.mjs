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

// right-release's own per-platform run lock (`.right-release/locks/*`, every platform) is
// written before the later clean-tree preflights run. Only untracked files directly in that lock directory are
// exempt, so repos that do not ignore `.right-release/` still pass, and nothing else in
// the vault (sealed output, receipts, state) or elsewhere is ever excused.
const OWN_LOCK_RECORD = /^\?\? (?:[^\0]*\/)?\.right-release\/locks\/[^\0/]+$/;

/** Remove right-release's own untracked lock-file records from NUL-separated porcelain v1 output. */
export function stripOwnLockRecords(status) {
  const records = status.split("\0");
  const kept = [];
  for (let index = 0; index < records.length; index += 1) {
    const record = records[index];
    if (!record) continue;
    if (OWN_LOCK_RECORD.test(record)) continue;
    kept.push(record);
    if (/^[RC]/.test(record) || /^.[RC]/.test(record)) kept.push(records[++index] ?? "");
  }
  return kept.length ? `${kept.join("\0")}\0` : "";
}

/** Path of one porcelain v1 record ("XY path"). Tolerates a record whose leading status column was
 * trimmed away by a caller ("M path"), so the first path never loses its first character (#782). */
function porcelainPath(entry) {
  const m = /^([ MADRCU?!]{2} |[MADRCU?!] )/.exec(entry);
  return m ? entry.slice(m[0].length) : entry;
}

export function assertCleanSource({ status, commandId = "release" }) {
  if (typeof status !== "string") throw new Error("release source status is required");
  status = stripOwnLockRecords(status);
  if (status.length !== 0) {
    const files = status.split("\0").filter(Boolean).map(porcelainPath).slice(0, 10);
    throw new Error(`${commandId} requires a clean working tree; commit or stash first:\n${files.map((file) => `- ${file}`).join("\n")}`);
  }
}
