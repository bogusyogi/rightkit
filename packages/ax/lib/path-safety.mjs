// Shared path-containment heuristics. Used by plugin/validate.mjs (agent-
// plugins mcp.json path safety, static rule 13) and ax/conformance.mjs
// (invariant I5 containment check). Detects absolute, UNC, drive-relative,
// and parent-traversal path shapes — none of these can validly stay inside a
// bounded root — independent of whether the value also carries a
// ${PLACEHOLDER} token.
//
// A "bare token" (no path separators, no drive prefix) such as "node" or
// "python3" is deliberately never flagged: it is resolved via PATH, exactly
// like invoking an installed CLI, and has no path shape to contain.
//
// F8 fix: lexical hazard detection (above) and lexical resolve()+startsWith
// containment are both blind to a directory symlink that lives inside the
// root but resolves (readlink-follows) outside it — the lexical path stays
// "inside", the real filesystem path does not. resolvesOutsideRoot() is the
// property-level fix: it canonicalizes both the root and the candidate via
// fs.realpathSync and re-checks containment against the REAL paths, so a
// symlinked directory anywhere in the chain is caught. It is additive to the
// lexical check (a lexical escape is still an escape), and it degrades
// gracefully — a candidate/root that does not exist on disk yet has no real
// path to canonicalize, so the function falls back to the lexical result
// rather than throwing.

import { realpathSync } from "node:fs";
import { resolve, sep } from "node:path";

const HAS_PLACEHOLDER = /\$\{[A-Z_]+\}/;
const PARENT_SEGMENT = /(^|[\\/])\.\.(?:[\\/]|$)/;
const UNC_PATH = /^\\\\/;
const WINDOWS_ABSOLUTE = /^[a-zA-Z]:[\\/]/;
const WINDOWS_DRIVE_RELATIVE = /^[a-zA-Z]:(?![\\/])/;
const POSIX_ABSOLUTE = /^\//;

function isLexicallyContained(resolvedCandidate, normalizedRoot) {
  return resolvedCandidate === normalizedRoot || resolvedCandidate.startsWith(normalizedRoot + sep);
}

// F8: the property-level containment check. `candidatePath` is resolved
// against `root` exactly like the lexical checks always did, but containment
// is ALSO re-verified against the realpath'd (symlink-resolved) forms of
// both sides, so a directory symlink anywhere in candidatePath's chain that
// points outside root cannot lexically-pass and then be spawned/read
// anyway. Returns true (escapes) on either signal firing.
export function resolvesOutsideRoot(candidatePath, root) {
  const normalizedRoot = resolve(root);
  const resolvedCandidate = resolve(root, candidatePath);
  if (!isLexicallyContained(resolvedCandidate, normalizedRoot)) return true;

  let realRoot;
  try { realRoot = realpathSync(normalizedRoot); } catch { return false; } // root doesn't exist on disk (yet) — nothing further to canonicalize
  let realCandidate;
  try { realCandidate = realpathSync(resolvedCandidate); } catch { return false; } // candidate doesn't exist on disk (yet) — lexical containment already held
  return !isLexicallyContained(realCandidate, realRoot);
}

export function isBareToken(value) {
  return typeof value === "string" && value.length > 0 && !/[\\/]/.test(value) && !WINDOWS_DRIVE_RELATIVE.test(value) && !WINDOWS_ABSOLUTE.test(value);
}

export function hasPlaceholder(value) {
  return typeof value === "string" && HAS_PLACEHOLDER.test(value);
}

// Returns a short hazard label for a raw (unexpanded) path-shaped string, or
// null if it carries no recognizable hazard shape.
export function describePathHazard(value) {
  if (typeof value !== "string" || !value) return null;
  if (UNC_PATH.test(value)) return "UNC path";
  if (WINDOWS_ABSOLUTE.test(value)) return "absolute (drive-letter) path";
  if (WINDOWS_DRIVE_RELATIVE.test(value)) return "drive-relative path";
  if (POSIX_ABSOLUTE.test(value)) return "absolute path";
  if (PARENT_SEGMENT.test(value)) return "parent-directory ('..') traversal";
  return null;
}
