// Static inventory of the places the release tool refuses a release: fail(, reject(, throw new ...Error and process.exit(1).
// doctor-parity.test.mjs classifies every site in refusals.json; a site that is not listed fails the test, so a new refusal
// cannot appear unclassified. Key: file::enclosingFunction::sha8(normalised message template). The scanner is a small
// tokenizer (no parser dependency): comments, strings, template literals and regex literals are skipped, braces and parens are
// tracked to name the enclosing function and the enclosing check("name", ...) of a site.
import { createHash } from "node:crypto";

const KEYWORDS_BEFORE_REGEX = new Set(["return", "typeof", "case", "do", "else", "in", "of", "new", "delete", "void", "throw", "await", "yield"]);
const CONTROL_KEYWORDS = new Set(["if", "for", "while", "switch", "catch", "with", "function", "return", "typeof", "new", "else", "do"]);
const REGEX_PREV_PUNCT = new Set(["(", ",", "=", ":", "[", "!", "&", "|", "?", "{", "}", ";", "+", "-", "*", "%", "<", ">", "~", "^", "=>", "&&", "||", "??", "==", "===", "!=", "!==", "+=", "-=", "=="]);
const PUNCT3 = new Set(["===", "!==", "**=", "...", "<<=", ">>=", ">>>", "&&=", "||=", "??="]);
const PUNCT2 = new Set(["=>", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "**", "<<", ">>"]);
// A class named like these is a refusal when thrown; the scanner takes every `new <Name>Error(` and ReleaseRefusal.
const REFUSAL_CLASS = /^(?:[A-Za-z0-9_]*Error|ReleaseRefusal)$/;

function skipTemplate(source, from) {
  // `from` is the index after the opening backtick; returns the index after the closing one.
  let i = from;
  while (i < source.length) {
    const ch = source[i];
    if (ch === "\\") { i += 2; continue; }
    if (ch === "`") return i + 1;
    if (ch === "$" && source[i + 1] === "{") { i = skipInterpolation(source, i + 2); continue; }
    i += 1;
  }
  return i;
}

function skipInterpolation(source, from) {
  // `from` is the index after "${"; returns the index after the matching "}".
  let depth = 1;
  let i = from;
  while (i < source.length && depth > 0) {
    const ch = source[i];
    if (ch === "{") depth += 1;
    else if (ch === "}") depth -= 1;
    else if (ch === "'" || ch === "\"") i = skipQuoted(source, i + 1, ch) - 1;
    else if (ch === "`") i = skipTemplate(source, i + 1) - 1;
    else if (ch === "/" && source[i + 1] === "/") { while (i < source.length && source[i] !== "\n") i += 1; continue; }
    else if (ch === "/" && source[i + 1] === "*") { const end = source.indexOf("*/", i + 2); i = end < 0 ? source.length : end + 1; }
    i += 1;
  }
  return i;
}

function skipQuoted(source, from, quote) {
  let i = from;
  while (i < source.length) {
    const ch = source[i];
    if (ch === "\\") { i += 2; continue; }
    if (ch === quote || ch === "\n") return i + 1;
    i += 1;
  }
  return i;
}

function skipRegex(source, from) {
  // `from` is the index after the opening slash; returns the index after the flags.
  let i = from;
  let inClass = false;
  while (i < source.length) {
    const ch = source[i];
    if (ch === "\\") { i += 2; continue; }
    if (ch === "\n") return i;
    if (ch === "[") inClass = true;
    else if (ch === "]") inClass = false;
    else if (ch === "/" && !inClass) { i += 1; break; }
    i += 1;
  }
  while (/[a-z]/i.test(source[i] ?? "")) i += 1;
  return i;
}

/** The text of a template literal with every ${...} interpolation blanked, so a message keeps its key when only an expression moves. */
function blankInterpolations(raw) {
  let out = "";
  let i = 0;
  while (i < raw.length) {
    if (raw[i] === "\\") { out += raw.slice(i, i + 2); i += 2; continue; }
    if (raw[i] === "$" && raw[i + 1] === "{") { out += "${}"; i = skipInterpolation(raw, i + 2); continue; }
    out += raw[i];
    i += 1;
  }
  return out;
}

export function tokenize(source) {
  const tokens = [];
  let i = 0;
  let line = 1;
  const prev = () => tokens[tokens.length - 1];
  const regexAllowed = () => {
    const last = prev();
    if (!last) return true;
    if (last.type === "punct") return REGEX_PREV_PUNCT.has(last.value);
    if (last.type === "id") return KEYWORDS_BEFORE_REGEX.has(last.value);
    return false;
  };
  const push = (type, value, start, end) => tokens.push({ type, value, start, end, line });
  while (i < source.length) {
    const ch = source[i];
    if (ch === "\n") { line += 1; i += 1; continue; }
    if (/\s/.test(ch)) { i += 1; continue; }
    if (ch === "/" && source[i + 1] === "/") { while (i < source.length && source[i] !== "\n") i += 1; continue; }
    if (ch === "/" && source[i + 1] === "*") {
      const end = source.indexOf("*/", i + 2);
      const stop = end < 0 ? source.length : end + 2;
      line += (source.slice(i, stop).match(/\n/g) ?? []).length;
      i = stop;
      continue;
    }
    if (ch === "#" && i === 0 && source[1] === "!") { while (i < source.length && source[i] !== "\n") i += 1; continue; }
    if (ch === "'" || ch === "\"") {
      const end = skipQuoted(source, i + 1, ch);
      push("str", source.slice(i, end), i, end);
      i = end;
      continue;
    }
    if (ch === "`") {
      const end = skipTemplate(source, i + 1);
      const raw = source.slice(i, end);
      push("tmpl", blankInterpolations(raw), i, end);
      line += (raw.match(/\n/g) ?? []).length;
      i = end;
      continue;
    }
    if (ch === "/" && regexAllowed()) {
      const end = skipRegex(source, i + 1);
      push("regex", source.slice(i, end), i, end);
      i = end;
      continue;
    }
    if (/[A-Za-z_$]/.test(ch)) {
      let end = i + 1;
      while (end < source.length && /[A-Za-z0-9_$]/.test(source[end])) end += 1;
      push("id", source.slice(i, end), i, end);
      i = end;
      continue;
    }
    if (/[0-9]/.test(ch)) {
      let end = i + 1;
      while (end < source.length && /[0-9A-Za-z_.]/.test(source[end])) end += 1;
      push("num", source.slice(i, end), i, end);
      i = end;
      continue;
    }
    const three = source.slice(i, i + 3);
    const two = source.slice(i, i + 2);
    const value = PUNCT3.has(three) ? three : PUNCT2.has(two) ? two : ch;
    push("punct", value, i, i + value.length);
    i += value.length;
  }
  return tokens;
}

const OPEN = new Map([["(", ")"], ["[", "]"], ["{", "}"]]);

function matchingClose(tokens, openIndex) {
  const stack = [];
  for (let i = openIndex; i < tokens.length; i += 1) {
    const t = tokens[i];
    if (t.type !== "punct") continue;
    if (OPEN.has(t.value)) stack.push(t.value);
    else if (t.value === ")" || t.value === "]" || t.value === "}") {
      stack.pop();
      if (stack.length === 0) return i;
    }
  }
  return -1;
}

function matchingOpen(tokens, closeIndex) {
  const stack = [];
  for (let i = closeIndex; i >= 0; i -= 1) {
    const t = tokens[i];
    if (t.type !== "punct") continue;
    if (t.value === ")" || t.value === "]" || t.value === "}") stack.push(t.value);
    else if (OPEN.has(t.value)) {
      stack.pop();
      if (stack.length === 0) return i;
    }
  }
  return -1;
}

const ANONYMOUS = Symbol("anonymous");

/** Which `{` tokens open a function body, and the name each is known by (ANONYMOUS for an unnamed callback). */
function functionBodies(tokens) {
  const bodies = new Map();
  for (let i = 0; i < tokens.length; i += 1) {
    const t = tokens[i];
    if (t.type === "id" && t.value === "function") {
      let nameAt = i + 1;
      if (tokens[nameAt]?.value === "*") nameAt += 1;
      const name = tokens[nameAt]?.type === "id" ? tokens[nameAt].value : null;
      const open = name ? nameAt + 1 : nameAt;
      if (tokens[open]?.value !== "(") continue;
      const close = matchingClose(tokens, open);
      if (close >= 0 && tokens[close + 1]?.value === "{") bodies.set(close + 1, name ?? ANONYMOUS);
    } else if (t.type === "punct" && t.value === "=>" && tokens[i + 1]?.value === "{") {
      let name = ANONYMOUS;
      let j = i - 1;
      if (tokens[j]?.value === ")") j = matchingOpen(tokens, j) - 1;
      else if (tokens[j]?.type === "id") j -= 1;
      if (tokens[j]?.value === "async") j -= 1;
      if ((tokens[j]?.value === "=" || tokens[j]?.value === ":") && tokens[j - 1]?.type === "id") name = tokens[j - 1].value;
      bodies.set(i + 1, name);
    } else if (t.type === "id" && !CONTROL_KEYWORDS.has(t.value) && tokens[i + 1]?.value === "(" && !["function", ".", "new"].includes(tokens[i - 1]?.value)) {
      const close = matchingClose(tokens, i + 1);
      if (close >= 0 && tokens[close + 1]?.value === "{" && !bodies.has(close + 1)) bodies.set(close + 1, t.value);
    }
  }
  return bodies;
}

function argumentRanges(tokens, openIndex) {
  const close = matchingClose(tokens, openIndex);
  const ranges = [];
  let start = openIndex + 1;
  let depth = 0;
  for (let i = openIndex + 1; i < close; i += 1) {
    const t = tokens[i];
    if (t.type === "punct") {
      if (OPEN.has(t.value)) depth += 1;
      else if (t.value === ")" || t.value === "]" || t.value === "}") depth -= 1;
      else if (t.value === "," && depth === 0) { ranges.push([start, i]); start = i + 1; }
    }
  }
  if (close > start || ranges.length) ranges.push([start, close]);
  return ranges;
}

const sha8 = (text) => createHash("sha256").update(text).digest("hex").slice(0, 8);
const textOf = (tokens, [from, to]) => tokens.slice(from, to).map((t) => t.value).join(" ").replace(/\s+/g, " ").trim();
const literalOf = (token) => (token?.type === "str" ? token.value.slice(1, -1) : null);

/** Every refusal site of a source file: { key, kind, line, fn, check, name, message }. */
export function scanRefusalSiteDetails(sourceText, file) {
  const tokens = tokenize(sourceText);
  const bodies = functionBodies(tokens);
  const frames = [];
  const sites = [];
  const seen = new Map();
  const currentFunction = () => {
    for (let k = frames.length - 1; k >= 0; k -= 1) {
      const name = frames[k].fn;
      if (typeof name === "string") return name;
    }
    return "<module>";
  };
  const currentCheck = () => frames.findLast((frame) => frame.check)?.check ?? null;
  const add = (kind, at, message, name = null) => {
    const base = `${file}::${currentFunction()}::${sha8(message)}`;
    const count = (seen.get(base) ?? 0) + 1;
    seen.set(base, count);
    sites.push({ key: count === 1 ? base : `${base}#${count}`, kind, line: tokens[at].line, fn: currentFunction(), check: currentCheck(), name, message });
  };
  for (let i = 0; i < tokens.length; i += 1) {
    const t = tokens[i];
    if (t.type === "punct") {
      if (OPEN.has(t.value)) {
        const frame = { fn: t.value === "{" ? (bodies.get(i) === ANONYMOUS ? null : bodies.get(i) ?? null) : null, check: null };
        if (t.value === "(" && tokens[i - 1]?.type === "id" && (tokens[i - 1].value === "check" || tokens[i - 1].value === "rejectBlocked")) {
          const name = literalOf(tokens[i + 1]);
          if (name) frame.check = name;
        }
        frames.push(frame);
      } else if (t.value === ")" || t.value === "]" || t.value === "}") frames.pop();
      continue;
    }
    if (t.type !== "id") continue;
    const before = tokens[i - 1]?.value;
    if ((t.value === "fail" || t.value === "reject") && tokens[i + 1]?.value === "(" && before !== "function" && before !== ".") {
      const args = argumentRanges(tokens, i + 1);
      // reject(name, detail) records a doctor finding; reject(error) alone is a Promise rejection, not a refusal.
      if (t.value === "reject" && args.length < 2) continue;
      const messageRange = t.value === "reject" ? args[1] : args[0];
      if (!messageRange) continue;
      add(t.value, i, textOf(tokens, messageRange), t.value === "reject" ? literalOf(tokens[args[0][0]]) : null);
    } else if (t.value === "throw" && tokens[i + 1]?.value === "new" && tokens[i + 2]?.type === "id" && REFUSAL_CLASS.test(tokens[i + 2].value) && tokens[i + 3]?.value === "(") {
      const args = argumentRanges(tokens, i + 3);
      add("throw", i, `${tokens[i + 2].value} ${args[0] ? textOf(tokens, args[0]) : ""}`.trim());
    } else if (t.value === "process" && tokens[i + 1]?.value === "." && tokens[i + 2]?.value === "exit" && tokens[i + 3]?.value === "(" && tokens[i + 4]?.value === "1" && tokens[i + 5]?.value === ")") {
      add("exit", i, "process.exit(1)");
    }
  }
  return sites;
}

/** The keys of every refusal site of a source file, in source order. */
export function scanRefusalSites(sourceText, file) {
  return scanRefusalSiteDetails(sourceText, file).map((site) => site.key);
}
