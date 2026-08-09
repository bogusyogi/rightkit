// Redacts credential-shaped substrings from text before it is embedded in any
// report artifact (F9 remediation: conformance probe samples/error messages
// must never carry raw secret material — P19, the same property static rule
// 11 checks for lexically). Heuristic and non-exhaustive by construction: it
// catches common provider key shapes and generic key/token/secret
// assignments, which is what the live probe path (mcp-stdio-client.mjs,
// ax/conformance.mjs) actually observes flowing back from a target process.

// Whole-match patterns: the entire match is replaced with "[REDACTED]".
const WHOLE_MATCH_PATTERNS = [
  /\bsk-[A-Za-z0-9_-]{6,}\b/g, // OpenAI-style API keys
  /\bBearer\s+[A-Za-z0-9._-]{10,}\b/gi, // bearer tokens in Authorization-shaped text
  /\bAKIA[0-9A-Z]{16}\b/g, // AWS access key IDs
  /\bghp_[A-Za-z0-9]{20,}\b/g, // GitHub personal access tokens
  /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b/g, // JWT-shaped tokens
];
// Prefix-preserving pattern: group 1 (the "key: " / "token=" label) is kept,
// only the value after it is redacted — this one legitimately needs the
// capture-group callback form; mixing it into WHOLE_MATCH_PATTERNS would
// make String.replace's callback signature (match, ...groups, offset,
// string) misread the numeric offset as the "prefix" for the group-less
// patterns above.
const KEY_VALUE_PATTERN = /\b((?:api[_-]?key|secret|token|password|passwd)\s*[:=]\s*)["']?[A-Za-z0-9._-]{8,}["']?/gi;

const ALL_PATTERNS = [...WHOLE_MATCH_PATTERNS, KEY_VALUE_PATTERN];

export function redactText(value) {
  if (typeof value !== "string" || !value) return value;
  let result = value;
  for (const pattern of WHOLE_MATCH_PATTERNS) result = result.replace(pattern, "[REDACTED]");
  result = result.replace(KEY_VALUE_PATTERN, (_match, prefix) => `${prefix}[REDACTED]`);
  return result;
}

export function containsSecretShapedString(value) {
  if (typeof value !== "string" || !value) return false;
  return ALL_PATTERNS.some((pattern) => new RegExp(pattern.source, pattern.flags).test(value));
}
