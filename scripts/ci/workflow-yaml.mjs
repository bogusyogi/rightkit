// A strict parser for the YAML subset GitHub Actions workflows in this repository use: block
// mappings, block sequences, literal block scalars (`|` and `|-`, no indentation indicator), flow
// sequences of scalars on one line, quoted and plain single-line scalars, comments. It parses
// structure so a contract test can assert on it. It REJECTS everything it does not implement exactly
// instead of guessing, so an unparsed construct can never pass as "absent" and no scalar can mean
// something different to GitHub than to the contract: tabs anywhere, folded scalars (`>`) in any
// form, block scalars with an indentation indicator or `+` chomping, anchors, aliases, tags, merge
// keys, explicit keys, document markers, flow mappings (except the empty `{}`), flow collections that span lines, multi-line
// scalars, duplicate keys. Every scalar is returned as a string. Literal block scalars follow the
// spec: `|` keeps one trailing newline, `|-` none.

function fail(no, message) { throw new Error(`yaml line ${no}: ${message}`); }

function stripComment(text) {
  let quote = null;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (quote) { if (c === quote) quote = null; continue; }
    if ((c === '"' || c === "'") && (i === 0 || /[\s[,:]/.test(text[i - 1]))) { quote = c; continue; }
    if (c === "#" && (i === 0 || /\s/.test(text[i - 1]))) return text.slice(0, i).trimEnd();
  }
  return text.trimEnd();
}

function scalar(raw, no) {
  const text = raw.trim();
  if (text === "") return "";
  if (/^[&*!]/.test(text)) fail(no, `anchors, aliases and tags are not supported: ${text.slice(0, 40)}`);
  if (/^[|>]/.test(text)) fail(no, `block scalar header ${JSON.stringify(text.slice(0, 12))} is not supported here (only "|" and "|-" after a mapping key)`);
  if (/^(?:[-?:](?:\s|$)|[%@`])/.test(text)) fail(no, `YAML indicator at the start of a plain scalar is not supported: ${text.slice(0, 40)}`);
  // The empty flow mapping alone (`permissions: {}`) is unambiguous; any other flow mapping is refused.
  if (text === "{}") return {};
  if (text.startsWith("{")) fail(no, "flow mappings are not supported");
  if (text.startsWith('"')) {
    if (!/^"(?:[^"\\]|\\.)*"$/.test(text)) fail(no, `unterminated or trailing text after a quoted scalar: ${text.slice(0, 40)}`);
    return JSON.parse(text);
  }
  if (text.startsWith("'")) {
    if (!/^'(?:[^']|'')*'$/.test(text)) fail(no, `unterminated or trailing text after a quoted scalar: ${text.slice(0, 40)}`);
    return text.slice(1, -1).replaceAll("''", "'");
  }
  if (text.startsWith("[")) {
    if (!text.endsWith("]")) fail(no, "flow sequence must close on the same line");
    const inner = text.slice(1, -1);
    const items = []; let current = ""; let quote = null;
    for (const c of inner) {
      if (quote) { current += c; if (c === quote) quote = null; continue; }
      if (c === '"' || c === "'") { quote = c; current += c; continue; }
      if (c === "[" || c === "{") fail(no, "nested flow collections are not supported");
      if (c === ",") { items.push(current); current = ""; continue; }
      current += c;
    }
    if (quote) fail(no, "unterminated quote in flow sequence");
    items.push(current);
    const kept = items.map((item) => item.trim());
    while (kept.length && kept.at(-1) === "") kept.pop(); // a trailing comma
    if (kept.includes("")) fail(no, "empty item in a flow sequence");
    return kept.map((item) => scalar(item, no));
  }
  return text;
}

export function parseYaml(source) {
  const raw = String(source).replace(/\r\n/g, "\n").split("\n");
  raw.forEach((line, i) => { if (line.includes("\t")) fail(i + 1, "tabs are not allowed"); });
  const lines = raw.map((text, i) => ({ no: i + 1, raw: text, indent: /^( *)/.exec(text)[1].length, body: stripComment(text).trim() }));
  let cursor = 0;

  const skipBlank = () => { while (cursor < lines.length && lines[cursor].body === "") cursor += 1; };

  function blockScalar(parentIndent, header, no) {
    const strip = header === "|-";
    const body = [];
    let blockIndent = null;
    while (cursor < lines.length) {
      const line = lines[cursor];
      if (line.raw.trim() === "") {
        // A blank line deeper than the block indentation carries content (its spaces); not modeled.
        if (blockIndent !== null && line.raw.length > blockIndent) fail(line.no, "whitespace-only line inside a block scalar is indented past the block; not supported");
        body.push(""); cursor += 1; continue;
      }
      if (line.indent <= parentIndent) break;
      if (blockIndent === null) blockIndent = line.indent;
      if (line.indent < blockIndent) fail(line.no, "block scalar line is indented less than its first line");
      body.push(line.raw.slice(blockIndent));
      cursor += 1;
    }
    while (body.length && body.at(-1) === "") body.pop();
    if (body.length === 0) return "";
    return body.join("\n") + (strip ? "" : "\n");
  }

  function parseBlock(indent) {
    skipBlank();
    if (cursor >= lines.length) return null;
    const first = lines[cursor];
    if (first.indent !== indent) fail(first.no, `expected indent ${indent}, found ${first.indent}`);
    return /^-(\s|$)/.test(first.body) ? parseSequence(indent) : parseMapping(indent);
  }

  function parseSequence(indent) {
    const out = [];
    for (;;) {
      skipBlank();
      if (cursor >= lines.length) break;
      const line = lines[cursor];
      if (line.indent < indent) break;
      if (line.indent > indent) fail(line.no, "unexpected indentation inside a sequence");
      if (!/^-(\s|$)/.test(line.body)) break;
      const rest = line.body.slice(1);
      const offset = rest.length - rest.trimStart().length;
      const content = rest.trimStart();
      if (content === "") {
        cursor += 1; skipBlank();
        const next = lines[cursor];
        if (!next || next.indent <= indent) { out.push(""); continue; }
        out.push(parseBlock(next.indent));
        continue;
      }
      // "- key: value" opens a mapping whose keys sit at the column after "- ".
      if (keyMatch(content) && !/^["'[]/.test(content)) {
        const childIndent = indent + 1 + offset;
        lines[cursor] = { ...line, indent: childIndent, body: content, raw: " ".repeat(childIndent) + content };
        out.push(parseMapping(childIndent));
        continue;
      }
      cursor += 1;
      out.push(scalar(content, line.no));
    }
    return out;
  }

  function keyMatch(body) {
    const m = /^("(?:[^"\\]|\\.)*"|'(?:[^']|'')*'|[^\s:#"'[\]{},&*!|>-][^:]*?|-[^\s:][^:]*?)\s*:(?:\s+(.*))?$/.exec(body);
    return m ? { key: m[1], value: m[2] ?? "" } : null;
  }

  function parseMapping(indent) {
    const out = {};
    for (;;) {
      skipBlank();
      if (cursor >= lines.length) break;
      const line = lines[cursor];
      if (line.indent < indent) break;
      if (line.indent > indent) fail(line.no, "unexpected indentation inside a mapping");
      if (/^-(\s|$)/.test(line.body)) break;
      const hit = keyMatch(line.body);
      if (!hit) fail(line.no, `cannot parse mapping entry: ${line.body.slice(0, 60)}`);
      const key = hit.key.startsWith('"') || hit.key.startsWith("'") ? scalar(hit.key, line.no) : hit.key;
      if (Object.hasOwn(out, key)) fail(line.no, `duplicate key ${JSON.stringify(key)}`);
      cursor += 1;
      const value = hit.value.trim();
      if (value === "") {
        skipBlank();
        const next = lines[cursor];
        if (next && next.indent > indent) out[key] = parseBlock(next.indent);
        else if (next && next.indent === indent && /^-(\s|$)/.test(next.body)) out[key] = parseSequence(indent);
        else out[key] = "";
      } else if (value === "|" || value === "|-") {
        out[key] = blockScalar(indent, value, line.no);
      } else if (/^[|>]/.test(value)) {
        fail(line.no, `block scalar header ${JSON.stringify(value.slice(0, 12))} is not supported (indentation indicators, "+" chomping and folded scalars are rejected)`);
      } else {
        const next = (skipBlank(), lines[cursor]);
        if (next && next.indent > indent) fail(next.no, "multi-line plain scalars are not supported");
        out[key] = scalar(value, line.no);
      }
    }
    return out;
  }

  skipBlank();
  const result = cursor >= lines.length ? {} : parseBlock(lines[cursor].indent);
  skipBlank();
  if (cursor < lines.length) fail(lines[cursor].no, "unparsed trailing content");
  return result;
}
