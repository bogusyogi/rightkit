const words = (text) => (String(text ?? "").toLowerCase().match(/[\p{L}\p{N}_'-]+/gu) ?? []);
const normalized = (text) => String(text ?? "").replace(/\s+/g, " ").trim();

// Spoken renderings of punctuation that the product contract REQUIRES us to
// compact away ("https colon slash slash ..." -> "https://..."). They inflate
// the input token count without contributing content, so counting them in the
// deletion denominator hard-fails correct normalization. See
// docs/L1_TRAINING_RUN_2026-07-23.md, root cause 2.
const SPOKEN_FORM_TOKENS = new Set(["dot", "slash", "backslash", "colon", "semicolon", "at", "dash", "hyphen", "underscore", "equals", "period", "plus", "star", "asterisk", "tilde", "percent", "ampersand", "hash"]);

export function validateOutput(row) {
  const input = normalized(row.input_text);
  const output = normalized(row.output_text);
  const errors = [];
  if (!output) errors.push("empty: output is empty");
  if (row.finish_reason === "length") errors.push("truncation: generation stopped at the token cap");
  if (/[\u2013\u2014]/u.test(output)) errors.push("punctuation: forbidden em/en dash");
  if (/^(output|answer|corrected(?: text| transcript)?|here(?: is|'s))\s*:/iu.test(output) || /^###\s*(input|output)/iu.test(output)) errors.push("wrapper: output includes commentary or a prompt wrapper");
  const protectedSpans = row.structured_spans?.protected ?? row.protected_spans ?? [];
  for (const span of protectedSpans) {
    const target = span.target_surface ?? span.text ?? span.input_surface;
    if ((span.required ?? true) && target && !output.includes(target)) errors.push(`protected: missing ${target}`);
  }
  const inputTokens = words(input), outputTokens = words(output);
  const dictatedTokens = inputTokens.filter((token) => !SPOKEN_FORM_TOKENS.has(token));
  if (dictatedTokens.length >= 16 && outputTokens.length < Math.ceil(dictatedTokens.length * 0.55)) errors.push("deletion: output retains less than 55% of dictated input tokens");
  const counts = new Map();
  for (const token of outputTokens) counts.set(token, (counts.get(token) ?? 0) + 1);
  if (outputTokens.length >= 8 && [...counts.values()].some((count) => count >= Math.max(6, Math.ceil(outputTokens.length * 0.45)))) errors.push("loop: excessive token repetition");
  const context = row.bounded_context ?? {};
  const referenceText = [context.document_selection_excerpt, context.recent_repaired_text, ...(context.visible_labels ?? []), ...(context.nearby_accessibility_nodes ?? []).map((node) => node?.text)].filter(Boolean);
  for (const text of referenceText) {
    const phrase = normalized(text);
    if (phrase.length >= 24 && output.includes(phrase) && !input.includes(phrase)) { errors.push("invention: output copied reference-only context"); break; }
  }
  return { ok: errors.length === 0, errors };
}

const directCli = process.argv[1]
  && !process.execArgv.some((argument) => argument === "-e" || argument === "--eval")
  && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (directCli) {
  let input = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => { input += chunk; });
  process.stdin.on("end", () => process.stdout.write(`${JSON.stringify(validateOutput(JSON.parse(input)))}\n`));
}
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
