import { defaultTreeAdapter as base, parseFragment, type DefaultTreeAdapterMap, type DefaultTreeAdapterTypes as Nodes, type TreeAdapter } from "parse5";

export class ShikiSanitizationError extends Error {
  constructor(readonly code: "limit" | "syntax" | "structure" | "style") {
    super(`Shiki HTML refused (${code})`);
    this.name = "ShikiSanitizationError";
  }
}

const MAX_BYTES = 4_194_304;
const MAX_NODES = 65_536;
const MAX_LINES = 16_384;
const encoder = new TextEncoder();
function fail(code: ShikiSanitizationError["code"]): never { throw new ShikiSanitizationError(code); }

/** Linear work preflight, not an HTML parser. Bound tags/attributes BEFORE parse5's
 * tokenizer can perform its prior-attribute lookup. Semantic parsing still follows.
 */
function lexicalWorkGuard(input: string): void {
  let cursor = 0;
  let scanned = 0;
  let tagStart: number | undefined;
  const advance = (): string => {
    if (cursor === input.length) fail("syntax");
    if (++scanned > MAX_BYTES) fail("limit");
    const char = input[cursor++];
    if (tagStart !== undefined && cursor - tagStart > 2048) fail("limit");
    return char;
  };
  const space = (char: string | undefined) => char === " " || char === "\t" || char === "\n" || char === "\r" || char === "\f";
  const name = (): string => {
    let result = "";
    while (cursor < input.length && input[cursor] >= "a" && input[cursor] <= "z") {
      if (result.length === 8) fail("structure");
      result += advance();
    }
    if (!result) fail("syntax");
    return result;
  };
  while (cursor < input.length) {
    if (input[cursor] !== "<") { advance(); continue; }
    tagStart = cursor;
    advance();
    const closing = input[cursor] === "/";
    if (closing) advance();
    const tag = name();
    if (tag !== "pre" && tag !== "code" && tag !== "span") fail("structure");
    if (closing) {
      if (advance() !== ">") fail("syntax");
      tagStart = undefined;
      continue;
    }
    const seen = new Set<string>();
    while (input[cursor] !== ">") {
      if (!space(input[cursor])) fail("syntax");
      while (space(input[cursor])) advance();
      if (input[cursor] === ">") break;
      const attr = name();
      if (tag === "code" || (tag === "pre" ? !["class", "style", "tabindex"].includes(attr) : !["class", "style"].includes(attr))
        || seen.has(attr) || seen.size === (tag === "pre" ? 3 : 1)) fail("structure");
      seen.add(attr);
      while (space(input[cursor])) advance();
      if (advance() !== "=") fail("syntax");
      while (space(input[cursor])) advance();
      if (advance() !== '"') fail("syntax");
      while (input[cursor] !== '"') {
        // A quoted > is data, never a tag boundary. Raw < is never producer syntax.
        if (advance() === "<") fail("syntax");
      }
      advance();
    }
    advance();
    tagStart = undefined;
  }
}

function budgetAdapter(): TreeAdapter<DefaultTreeAdapterMap> {
  let nodes = 0;
  let depth = 0;
  const count = () => { if (++nodes > MAX_NODES) fail("limit"); };
  const adapter: TreeAdapter<DefaultTreeAdapterMap> = {
    ...base,
    createDocument() { count(); return base.createDocument(); },
    createDocumentFragment() { count(); return base.createDocumentFragment(); },
    createElement(...args) { count(); return base.createElement(...args); },
    createCommentNode(text) { count(); return base.createCommentNode(text); },
    createTextNode(text) { count(); return base.createTextNode(text); },
    setDocumentType(document, name, publicId, systemId) {
      if (!document.childNodes.some(base.isDocumentTypeNode)) count();
      base.setDocumentType(document, name, publicId, systemId);
    },
    // Default adapter calls its own createTextNode, bypassing a spread wrapper.
    insertText(parent, text) {
      const previous = parent.childNodes.at(-1);
      if (previous && base.isTextNode(previous)) previous.value += text;
      else adapter.appendChild(parent, adapter.createTextNode(text));
    },
    insertTextBefore(parent, text, reference) {
      const previous = parent.childNodes[parent.childNodes.indexOf(reference) - 1];
      if (previous && base.isTextNode(previous)) previous.value += text;
      else adapter.insertBefore(parent, adapter.createTextNode(text), reference);
    },
    onItemPush() { if (++depth > 8) fail("limit"); },
    onItemPop() { depth--; },
  };
  return adapter;
}

type ColorKey = "--shiki-light" | "--shiki-dark" | "--shiki-light-bg" | "--shiki-dark-bg";
type FontKind = "font-style" | "font-weight" | "text-decoration";
type StyleKey = ColorKey | `--shiki-${"light" | "dark"}-${FontKind}`;
type Style = Partial<Record<StyleKey, string>>;
interface Token { text: string; style?: Style }
interface Profile { style: Style; lines: Token[][] }
const ROOT_KEYS: readonly ColorKey[] = ["--shiki-light", "--shiki-dark", "--shiki-light-bg", "--shiki-dark-bg"];
const FONT_KINDS: readonly FontKind[] = ["font-style", "font-weight", "text-decoration"];
const STYLE_KEYS: readonly StyleKey[] = [...ROOT_KEYS, ...FONT_KINDS.flatMap((kind) => [`--shiki-light-${kind}`, `--shiki-dark-${kind}`] as const)];
const COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
function styleRecord(value: string, root: boolean): Style {
  if (!value || value.length > 512) fail(value.length > 512 ? "limit" : "style");
  const result: Style = {};
  for (const declaration of value.split(";")) {
    const parts = declaration.split(":");
    if (parts.length !== 2) fail("style");
    const [key, item] = parts as [StyleKey, string];
    if (!STYLE_KEYS.includes(key) || Object.hasOwn(result, key)) fail("style");
    if (ROOT_KEYS.includes(key as ColorKey)) {
      if ((!root && key.endsWith("-bg")) || !COLOR.test(item)) fail("style");
    } else {
      if (root) fail("style");
      const valid = key.endsWith("-font-style") ? ["italic", "inherit"]
        : key.endsWith("-font-weight") ? ["bold", "inherit"]
          : ["underline", "line-through", "underline line-through", "inherit"];
      if (!valid.includes(item)) fail("style");
    }
    result[key] = item;
  }
  for (const key of root ? ROOT_KEYS : ROOT_KEYS.slice(0, 2)) if (!result[key]) fail("style");
  if (!root) for (const kind of FONT_KINDS) {
    const light = result[`--shiki-light-${kind}`];
    const dark = result[`--shiki-dark-${kind}`];
    if ((light === undefined) !== (dark === undefined) || (light === "inherit" && dark === "inherit")) fail("style");
  }
  return result;
}

function element(node: Nodes.ChildNode | undefined, tag: "pre" | "code" | "span"): Nodes.Element {
  if (!node || !base.isElementNode(node) || node.tagName !== tag || node.namespaceURI !== "http://www.w3.org/1999/xhtml") fail("structure");
  return node;
}
function attributes(node: Nodes.Element, allowed: readonly string[]): Record<string, string> {
  const result: Record<string, string> = Object.create(null);
  for (const attr of node.attrs) {
    if (attr.namespace || attr.prefix || !allowed.includes(attr.name) || Object.hasOwn(result, attr.name)) fail("structure");
    if ((attr.name === "style" || attr.name === "class") && attr.value.length > 512) fail("limit");
    result[attr.name] = attr.value;
  }
  return result;
}

/** Validate every source interval: parsing must neither repair nor discard input. */
function sourcePartition(input: string, node: Nodes.ChildNode, depth: number): void {
  if (depth > 4) fail("limit");
  const loc = node.sourceCodeLocation;
  if (!loc || loc.startOffset < 0 || loc.endOffset > input.length) fail("syntax");
  if (base.isTextNode(node)) {
    if (input.slice(loc.startOffset, loc.endOffset).includes("<")) fail("syntax");
    return;
  }
  if (!base.isElementNode(node)) fail("structure");
  const bounds = node.sourceCodeLocation;
  if (!bounds?.startTag || !bounds.endTag || bounds.startOffset !== bounds.startTag.startOffset || bounds.endOffset !== bounds.endTag.endOffset
    || input.slice(bounds.endTag.startOffset, bounds.endTag.endOffset) !== `</${node.tagName}>`
    || !input.slice(bounds.startTag.startOffset, bounds.startTag.endOffset).startsWith(`<${node.tagName}`)) fail("syntax");
  let cursor = bounds.startTag.endOffset;
  for (const child of node.childNodes) {
    if (child.sourceCodeLocation?.startOffset !== cursor) fail("syntax");
    sourcePartition(input, child, depth + (base.isElementNode(child) ? 1 : 0));
    cursor = child.sourceCodeLocation!.endOffset;
  }
  if (cursor !== bounds.endTag.startOffset) fail("syntax");
}

function profile(input: string): Profile {
  const fragment = parseFragment(input, {
    treeAdapter: budgetAdapter(), scriptingEnabled: false, sourceCodeLocationInfo: true,
    onParseError() { fail("syntax"); },
  });
  if (fragment.childNodes.length !== 1) fail("structure");
  const pre = element(fragment.childNodes[0], "pre");
  if (pre.sourceCodeLocation?.startOffset !== 0 || pre.sourceCodeLocation.endOffset !== input.length) fail("syntax");
  sourcePartition(input, pre, 1);
  const attrs = attributes(pre, ["class", "style", "tabindex"]);
  if (!/^shiki shiki-themes(?: [A-Za-z0-9_-]+)*$/.test(attrs.class ?? "") || attrs.tabindex !== "0" || !attrs.style) fail("structure");
  const style = styleRecord(attrs.style, true);
  if (pre.childNodes.length !== 1) fail("structure");
  const code = element(pre.childNodes[0], "code");
  attributes(code, []);
  const lines: Token[][] = [];
  for (let i = 0; i < code.childNodes.length; i++) {
    const node = code.childNodes[i];
    if (i % 2) {
      if (!base.isTextNode(node) || node.value !== "\n" || input.slice(node.sourceCodeLocation!.startOffset, node.sourceCodeLocation!.endOffset) !== "\n") fail("structure");
      continue;
    }
    if (lines.length === MAX_LINES) fail("limit");
    const line = element(node, "span");
    const lineAttrs = attributes(line, ["class"]);
    if (lineAttrs.class !== "line") fail("structure");
    const tokens: Token[] = [];
    for (const child of line.childNodes) {
      const span = element(child, "span");
      const tokenAttrs = attributes(span, ["style"]);
      if (span.childNodes.length > 1) fail("structure");
      const text = span.childNodes[0];
      if (text && !base.isTextNode(text)) fail("structure");
      tokens.push({ text: text ? (text as Nodes.TextNode).value : "", style: tokenAttrs.style === undefined ? undefined : styleRecord(tokenAttrs.style, false) });
    }
    lines.push(tokens);
  }
  if (!lines.length || code.childNodes.length % 2 === 0) fail("structure");
  return { style, lines };
}

/** Classic Shiki 4.5 dual-theme/defaultColor:false only. Throws without returning partial HTML.
 * On failure render ORIGINAL code as escaped text/React children, with a highlighting-failed notice.
 */
export function sanitizeShikiHtml(input: string): string {
  if (typeof input !== "string") fail("syntax");
  if (input.length > MAX_BYTES || encoder.encode(input).byteLength > MAX_BYTES) fail("limit");
  lexicalWorkGuard(input);
  const data = profile(input);
  let bytes = 0;
  const chunks: string[] = [];
  const append = (text: string) => {
    bytes += encoder.encode(text).byteLength;
    if (bytes > MAX_BYTES) fail("limit");
    chunks.push(text);
  };
  const escape = (text: string) => {
    // Incremental escaping prevents a large intermediate expanded output.
    let start = 0;
    while (start < text.length) {
      // Keep surrogate pairs together when computing UTF-8 budgets.
      let end = Math.min(start + 1024, text.length);
      if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1])) end--;
      append(text.slice(start, end).replace(/[&<>"']/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]!));
      start = end;
    }
  };
  const writeStyle = (style: Style) => {
    append(' style="');
    let separator = "";
    for (const key of STYLE_KEYS) if (style[key] !== undefined) {
      append(`${separator}${key}:`); escape(style[key]!); separator = ";";
    }
    append('"');
  };
  append('<pre class="rk-shiki-profile shiki shiki-themes"');
  writeStyle(data.style);
  append(' tabindex="0"><code>');
  data.lines.forEach((line, index) => {
    if (index) append("\n");
    append('<span class="line">');
    for (const token of line) {
      append("<span"); if (token.style) writeStyle(token.style); append(">"); escape(token.text); append("</span>");
    }
    append("</span>");
  });
  append("</code></pre>");
  return chunks.join("");
}
