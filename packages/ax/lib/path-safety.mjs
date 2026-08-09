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

const HAS_PLACEHOLDER = /\$\{[A-Z_]+\}/;
const PARENT_SEGMENT = /(^|[\\/])\.\.(?:[\\/]|$)/;
const UNC_PATH = /^\\\\/;
const WINDOWS_ABSOLUTE = /^[a-zA-Z]:[\\/]/;
const WINDOWS_DRIVE_RELATIVE = /^[a-zA-Z]:(?![\\/])/;
const POSIX_ABSOLUTE = /^\//;

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
