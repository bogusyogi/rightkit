import crypto from "node:crypto";

export function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

export function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

export function classifyAttemptFailure(stage) {
  return stage === "assistant_output" || stage === "materialization" ? "rejected" : "error";
}

function extractTag(text, tag) {
  const match = text.match(new RegExp(`<${tag}>\\s*([\\s\\S]*?)\\s*</${tag}>`, "i"));
  return match?.[1]?.trim() ?? "";
}

function extractHeader(text, name) {
  const match = text.match(new RegExp(`^${name.replace(/[.*+?^${}()|[\\]\\]/g, "\\$&")}:\\s*(.*)$`, "mi"));
  return match?.[1]?.trim() ?? "";
}

function extractVocabulary(text) {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.startsWith("User-saved terms (") || line.startsWith("Known mishearings ("))
    .join("\n");
}

export function parseCapturedRow(row) {
  const request = row?.request;
  if (!request || !Array.isArray(request.messages)) throw new Error("captured row has no request messages");
  const user = request.messages.find((message) => message?.role === "user")?.content;
  if (typeof user !== "string") throw new Error("captured row has no user content");
  const transcript = extractTag(user, "transcript");
  if (!transcript) throw new Error("captured row has no transcript tag");
  const context = {
    app: extractHeader(user, "Active app"),
    window: extractHeader(user, "Active window/title"),
    kind: extractHeader(user, "App kind"),
    writing: extractHeader(user, "Writing convention"),
    field_text: extractTag(user, "field_text"),
    selected_text: extractTag(user, "selected_text"),
    vocabulary: extractVocabulary(user),
  };
  const normalizedTranscript = transcript.toLocaleLowerCase("en-US").replace(/\s+/g, " ").trim();
  return {
    transcript,
    transcript_group_sha256: sha256(normalizedTranscript),
    context,
    request_sha256: sha256(stableJson(request)),
    captured_at: row.at ?? null,
    cloud_model: row.model ?? request.model ?? null,
    cloud_provider: row.provider ?? null,
    cloud_reference: extractCloudText(row.response),
    cloud_error: row.error ?? null,
  };
}

export function extractCloudText(response) {
  if (typeof response !== "string" || !response.trim()) return null;
  try {
    const parsed = JSON.parse(response);
    const content = parsed?.choices?.[0]?.message?.content ?? parsed?.choices?.[0]?.text;
    return typeof content === "string" && content.trim() ? content.trim() : null;
  } catch {
    return response.trim();
  }
}

function compactAppContext(context) {
  const lines = [];
  if (context.app) lines.push(`active_app: ${context.app}`);
  if (context.window) lines.push(`window_title: ${context.window}`);
  if (context.kind) lines.push(`app_kind: ${context.kind}`);
  if (context.writing) lines.push(`writing_convention: ${context.writing}`);
  return lines.join("\n");
}

function lineNumber(text) {
  return text.split(/\r?\n/).map((line, index) => `<lid:${index + 1}>${line}`).join("\n");
}

function renderTemplate(template, data) {
  const rendered = template.replace(/\{([a-z_]+)\}/g, (_, key) => data[key] ?? "");
  return rendered.replace(/^[ \t]+$/gm, "").replace(/\n{3,}/g, "\n\n").trim();
}

function renderRawTemplate(template, data) {
  return template.replace(/\{([a-z_]+)\}/g, (_, key) => data[key] ?? "");
}

export function maxTokensFor(transcript, policy) {
  if (policy === "compact_edits") return 256;
  if (policy === "sotto_full_text") {
    const words = transcript.trim().split(/\s+/).filter(Boolean).length;
    return Math.max(900, Math.ceil(words * 1.5));
  }
  const wordsBudget = transcript.trim().split(/\s+/).filter(Boolean).length + 160;
  const charBudget = Math.floor([...transcript].length / 3) + 160;
  return Math.min(1200, Math.max(128, wordsBudget, charBudget));
}

export function buildRequest(candidate, recipe, captured) {
  const data = {
    app_context: compactAppContext(captured.context),
    field_text: captured.context.field_text,
    selected_text: captured.context.selected_text,
    vocabulary_context: captured.context.vocabulary,
    transcript: captured.transcript,
    line_numbered_transcript: lineNumber(captured.transcript),
  };
  const user = `${renderTemplate(recipe.user, data)}${candidate.user_suffix ?? ""}`;
  const system = renderTemplate(recipe.system ?? "", data);
  let messages;
  if (recipe.message_layout === "single_user") {
    messages = [{ role: "user", content: system ? `${system}\n\n${user}` : user }];
  } else if (recipe.message_layout === "few_shot") {
    const examples = (recipe.examples ?? []).flatMap((example) => [
      { role: "user", content: renderTemplate(example.user, data) },
      { role: "assistant", content: renderTemplate(example.assistant, data) },
    ]);
    messages = [{ role: "system", content: system }, ...examples, { role: "user", content: user }];
  } else {
    messages = [{ role: "system", content: system }, { role: "user", content: user }];
  }
  const generation = recipe.generation ?? {};
  const body = {
    model: candidate.id,
    messages,
    temperature: generation.temperature ?? 0,
    top_k: generation.top_k ?? 1,
    top_p: generation.top_p ?? 1,
    repeat_penalty: generation.repeat_penalty ?? 1,
    max_tokens: maxTokensFor(captured.transcript, generation.max_tokens_policy),
    stream: false,
    cache_prompt: true,
  };
  if (candidate.chat_template_kwargs) body.chat_template_kwargs = candidate.chat_template_kwargs;
  if (recipe.message_layout === "raw_prompt") body.raw_prompt = renderRawTemplate(recipe.user, data);
  if (recipe.stop) body.stop = recipe.stop;
  return body;
}

function stripEnvelope(text) {
  let value = String(text ?? "").trim();
  const fence = value.match(/^```(?:json|text)?\s*([\s\S]*?)\s*```$/i);
  if (fence) value = fence[1].trim();
  value = value.replace(/^(?:corrected transcript|corrected text|final text|output)\s*:\s*/i, "");
  value = value.replace(/^<(?:corrected_transcript|corrected_text|output)>\s*/i, "");
  value = value.replace(/\s*<\/(?:corrected_transcript|corrected_text|output)>$/i, "");
  return value.trim();
}

function countOccurrences(haystack, needle) {
  if (!needle) return 0;
  let count = 0;
  let cursor = 0;
  while (cursor <= haystack.length) {
    const next = haystack.indexOf(needle, cursor);
    if (next === -1) break;
    count += 1;
    cursor = next + needle.length;
  }
  return count;
}

function replaceLiteralOnce(haystack, needle, replacement) {
  const index = haystack.indexOf(needle);
  if (index === -1) return haystack;
  return `${haystack.slice(0, index)}${replacement}${haystack.slice(index + needle.length)}`;
}

function materializeSubstringEdits(transcript, raw) {
  const parsed = JSON.parse(stripEnvelope(raw));
  if (!Array.isArray(parsed) || parsed.length > 64) throw new Error("edit output must be a JSON array with at most 64 items");
  let output = transcript;
  for (const [index, edit] of parsed.entries()) {
    if (!edit || typeof edit !== "object" || Object.keys(edit).some((key) => !["old", "new"].includes(key))) {
      throw new Error(`edit ${index} has an invalid shape`);
    }
    if (typeof edit.old !== "string" || typeof edit.new !== "string" || edit.old.length === 0) {
      throw new Error(`edit ${index} needs non-empty old and string new`);
    }
    if (countOccurrences(output, edit.old) !== 1) throw new Error(`edit ${index} old substring is not unique`);
    output = replaceLiteralOnce(output, edit.old, edit.new);
  }
  return output;
}

function splitCallArgs(text) {
  const args = [];
  let current = "";
  let quote = null;
  let escaped = false;
  for (const char of text) {
    if (escaped) { current += char; escaped = false; continue; }
    if (char === "\\") { current += char; escaped = true; continue; }
    if (quote) { current += char; if (char === quote) quote = null; continue; }
    if (char === "'" || char === '"') { quote = char; current += char; continue; }
    if (char === ",") { args.push(current.trim()); current = ""; continue; }
    current += char;
  }
  if (quote) throw new Error("unterminated quoted call argument");
  if (current.trim() || text.trim()) args.push(current.trim());
  return args;
}

function parseStringArg(value) {
  if (value.startsWith('"')) return JSON.parse(value);
  if (!value.startsWith("'") || !value.endsWith("'")) throw new Error("call string argument must be quoted");
  return value.slice(1, -1)
    .replace(/\\n/g, "\n")
    .replace(/\\r/g, "\r")
    .replace(/\\t/g, "\t")
    .replace(/\\'/g, "'")
    .replace(/\\\\/g, "\\");
}

function parseIntArg(value, label) {
  if (!/^\d+$/.test(value)) throw new Error(`${label} must be a positive integer`);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0) throw new Error(`${label} is out of range`);
  return parsed;
}

function materializeUltraX(transcript, raw) {
  const calls = stripEnvelope(raw).split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  if (calls.length === 0 || calls.length > 64) throw new Error("UltraX output needs 1-64 calls");
  if (calls.includes("keep_all()")) {
    if (calls.length !== 1) throw new Error("keep_all cannot be combined with edits");
    return transcript;
  }
  const lines = transcript.split(/\r?\n/);
  const removed = new Set();
  const additions = new Map();
  for (const call of calls) {
    const match = call.match(/^(replace_str|remove_lines|add_line)\((.*)\)$/);
    if (!match) throw new Error(`unsupported UltraX call: ${call}`);
    const args = splitCallArgs(match[2]);
    if (match[1] === "replace_str") {
      if (args.length !== 3) throw new Error("replace_str needs 3 arguments");
      const line = parseIntArg(args[0], "line");
      if (line < 1 || line > lines.length || removed.has(line)) throw new Error("replace_str line is invalid");
      const oldValue = parseStringArg(args[1]);
      const newValue = parseStringArg(args[2]);
      if (!oldValue || countOccurrences(lines[line - 1], oldValue) !== 1) throw new Error("replace_str old value is not unique on its line");
      lines[line - 1] = replaceLiteralOnce(lines[line - 1], oldValue, newValue);
    } else if (match[1] === "remove_lines") {
      if (args.length !== 2) throw new Error("remove_lines needs 2 arguments");
      const start = parseIntArg(args[0], "start");
      const end = parseIntArg(args[1], "end");
      if (start < 1 || end < start || end > lines.length) throw new Error("remove_lines range is invalid");
      for (let line = start; line <= end; line += 1) removed.add(line);
    } else {
      if (args.length !== 3) throw new Error("add_line needs 3 arguments");
      const base = parseIntArg(args[0], "base");
      const sub = parseIntArg(args[1], "sub_idx");
      if (base < 0 || base > lines.length) throw new Error("add_line base is invalid");
      const key = `${base}:${sub}`;
      if (additions.has(key)) throw new Error("duplicate add_line position");
      additions.set(key, parseStringArg(args[2]));
    }
  }
  const output = [];
  for (let base = 0; base <= lines.length; base += 1) {
    [...additions.entries()]
      .filter(([key]) => Number(key.split(":")[0]) === base)
      .sort(([a], [b]) => Number(a.split(":")[1]) - Number(b.split(":")[1]))
      .forEach(([, value]) => {
        output.push(value);
      });
    if (base > 0 && !removed.has(base)) output.push(lines[base - 1]);
  }
  return output.join("\n");
}

export function materializeOutput(transcript, raw, outputMode) {
  if (outputMode === "full_text_v1") return stripEnvelope(raw);
  if (outputMode === "substring_edits_v1") return materializeSubstringEdits(transcript, raw);
  if (outputMode === "ultrax_calls_v1") return materializeUltraX(transcript, raw);
  throw new Error(`unsupported output mode: ${outputMode}`);
}

function numericTokens(text) {
  return String(text).match(/\d+(?:[.,]\d+)*/g) ?? [];
}

function normalized(text) {
  return String(text).toLocaleLowerCase("en-US").replace(/\s+/g, " ").trim();
}

function lexicalTokens(text) {
  return String(text).toLocaleLowerCase("en-US").match(/[\p{L}\p{N}]+/gu) ?? [];
}

function retainedTokenRatio(source, output) {
  const remaining = new Map();
  for (const token of lexicalTokens(output)) remaining.set(token, (remaining.get(token) ?? 0) + 1);
  const sourceTokens = lexicalTokens(source);
  let retained = 0;
  for (const token of sourceTokens) {
    const count = remaining.get(token) ?? 0;
    if (count > 0) {
      retained += 1;
      remaining.set(token, count - 1);
    }
  }
  return { sourceTokens, ratio: sourceTokens.length ? retained / sourceTokens.length : 1 };
}

export function assertBenchmarkCorpusShape(shape) {
  const expected = { unique: 116, tune: 58, tuneA: 16, tuneB: 42, holdout: 58 };
  const mismatches = Object.entries(expected).filter(([key, value]) => shape?.[key] !== value);
  if (mismatches.length) {
    throw new Error(`corpus contract mismatch: ${mismatches.map(([key, value]) => `${key}=${shape?.[key]} (expected ${value})`).join(", ")}`);
  }
}

export function deterministicFlags(transcript, output, context = {}) {
  const flags = [];
  if (!output.trim()) flags.push("empty_output");
  if (stableJson(numericTokens(transcript)) !== stableJson(numericTokens(output))) flags.push("digit_sequence_changed");
  const sourceNorm = normalized(transcript);
  const outputNorm = normalized(output);
  if (sourceNorm.length >= 16 && outputNorm.length < sourceNorm.length * 0.55) flags.push("large_deletion");
  if (transcript.length >= 40 && output.length > transcript.length * 1.8) flags.push("large_expansion");
  const retention = retainedTokenRatio(transcript, output);
  const minimumRetention = retention.sourceTokens.length <= 6 ? 0.5 : 0.65;
  if (retention.sourceTokens.length >= 4 && retention.ratio < minimumRetention) flags.push("content_deletion");
  for (const [key, value] of [["field", context.field_text], ["selection", context.selected_text]]) {
    const valueNorm = normalized(value ?? "");
    if (valueNorm.length >= 20 && !sourceNorm.includes(valueNorm) && outputNorm.includes(valueNorm)) flags.push(`${key}_echo`);
  }
  if (/```|^\s*(?:here(?:'s| is)|sure[,!])/i.test(output)) flags.push("wrapper_or_preamble");
  return flags;
}

export function nearestRankPercentile(values, percentile) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(percentile * sorted.length) - 1)];
}
