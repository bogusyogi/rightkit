// Redacts credential-shaped substrings from text before it is embedded in any
// report artifact (F9 remediation: conformance probe samples/error messages
// must never carry raw secret material — P19, the same property static rule
// 11 checks for lexically). Heuristic and non-exhaustive by construction: it
// catches common provider key shapes and generic key/token/secret
// assignments, which is what the live probe path (mcp-stdio-client.mjs,
// ax/conformance.mjs) actually observes flowing back from a target process.
//
// F9 (round 2): a single `sk-` pattern only ever caught OpenAI/Anthropic-
// shaped keys — a GitHub fine-grained PAT or a Slack bot token flowing back
// from a probed server passed through untouched. The property being fixed
// is "known credential SHAPES are redacted", not "this one shape is
// redacted", so WHOLE_MATCH_PATTERNS is a growable SET covering the common
// shapes actually seen in the wild: OpenAI/Anthropic (`sk-...`, including
// `sk-ant-...`), GitHub classic + fine-grained PATs (`ghp_`/`gho_`/`ghs_`/
// `github_pat_`), Slack tokens (`xoxb-`/`xoxp-`/`xoxa-`/`xoxr-`/`xoxs-`), AWS
// access key IDs (`AKIA...`), Google API keys (`AIza...`), generic
// `Bearer <token>`, JWT-shaped tokens, and PEM private-key blocks. Ordinary
// text — including a harmless `key=value` (not one of the recognized
// key/secret/token/password label words below) — is left untouched.
//
// F9 (round 3): the bearer pattern required >=10 token characters, which let
// a short literal like "Bearer x" survive untouched — the property is "a
// Bearer scheme is followed by SOME credential material", not "at least 10
// characters of it", so the token requires only one-or-more characters now.
const WHOLE_MATCH_PATTERNS = [
  /\bsk-[A-Za-z0-9_-]{6,}\b/g, // OpenAI-style API keys, including Anthropic's sk-ant-... (same shape)
  /\bBearer\s+[A-Za-z0-9._-]+\b/gi, // bearer tokens in Authorization-shaped text, any non-empty token
  /\bAKIA[0-9A-Z]{16}\b/g, // AWS access key IDs
  /\bghp_[A-Za-z0-9]{20,}\b/g, // GitHub personal access tokens (classic)
  /\bgho_[A-Za-z0-9]{20,}\b/g, // GitHub OAuth access tokens
  /\bghs_[A-Za-z0-9]{20,}\b/g, // GitHub server-to-server tokens
  /\bgithub_pat_[A-Za-z0-9_]{20,}\b/g, // GitHub fine-grained personal access tokens
  /\bxox[baprs]-[A-Za-z0-9-]{10,}\b/g, // Slack tokens (bot/app/legacy/refresh/other)
  /\bAIza[A-Za-z0-9_-]{20,}\b/g, // Google API keys
  /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b/g, // JWT-shaped tokens
  /-----BEGIN(?: [A-Z]+)? PRIVATE KEY-----[\s\S]*?-----END(?: [A-Z]+)? PRIVATE KEY-----/g, // PEM private-key blocks
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
