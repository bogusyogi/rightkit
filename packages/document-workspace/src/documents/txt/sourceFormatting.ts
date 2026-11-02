import type { Diagnostic } from "@codemirror/lint";
import { hostAppName } from "../../appName.js";

export type SourceLanguage = "markdown" | "html" | "plain";

/** Pure core of the inline-wrap toolbar/shortcut action: wraps the selection [s,e) in
 *  `before`/`after`, keeping leading/trailing whitespace outside the markers and returning the new
 *  caret/selection. Exported so link/image/bold/italic wrapping is unit-testable without a DOM.
 */
export function applyInlineWrap(v: string, s: number, e: number, before: string, after: string) {
  const sel = v.slice(s, e);
  const lead = (sel.match(/^\s*/) ?? [""])[0];
  const trail = (sel.match(/\s*$/) ?? [""])[0];
  const core = sel.slice(lead.length, sel.length - trail.length);
  if (!core) {
    return {
      value: v.slice(0, s) + before + after + v.slice(e),
      start: s + before.length,
      end: s + before.length,
    };
  }
  const cs = s + lead.length + before.length;
  return {
    value: v.slice(0, s) + lead + before + core + after + trail + v.slice(e),
    start: cs,
    end: cs + core.length,
  };
}

/** Build a Markdown link/image token from the dialog's Display text + Address. Exported for tests. */
export function buildMarkdownLink(prefix: "[" | "![", text: string, url: string) {
  return `${prefix}${text || (prefix === "![" ? "image" : "link")}](${url})`;
}

const VOID_HTML_TAGS = new Set([
  "area",
  "base",
  "br",
  "col",
  "embed",
  "hr",
  "img",
  "input",
  "link",
  "meta",
  "param",
  "source",
  "track",
  "wbr",
]);

function htmlTagName(token: string): string | null {
  return token.match(/^<\/?\s*([a-zA-Z][\w:-]*)/)?.[1].toLowerCase() ?? null;
}

function isHtmlMetaToken(token: string): boolean {
  return /^<!|^<\?/.test(token) && !/^<!\[CDATA\[/i.test(token);
}

function isHtmlClosingTag(token: string): boolean {
  return /^<\//.test(token);
}

function isHtmlSelfClosing(token: string): boolean {
  const name = htmlTagName(token);
  return !name || /\/>\s*$/.test(token) || VOID_HTML_TAGS.has(name) || isHtmlMetaToken(token);
}

function indentLine(level: number, text: string): string {
  return `${"  ".repeat(Math.max(0, level))}${text}`;
}

export function formatHtmlSource(source: string): string {
  const tokens = source
    .replace(/\r\n?/g, "\n")
    .split(/(<!--[\s\S]*?-->|<!\[CDATA\[[\s\S]*?\]\]>|<![^>]*>|<\/?[^>]+>)/g)
    .map((token) => token.trim())
    .filter(Boolean);
  const lines: string[] = [];
  let level = 0;

  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    const isTag = token.startsWith("<") && token.endsWith(">");
    if (!isTag) {
      lines.push(indentLine(level, token));
      continue;
    }

    const name = htmlTagName(token);
    const next = tokens[i + 1];
    const afterNext = tokens[i + 2];
    if (
      !isHtmlClosingTag(token) &&
      name &&
      next &&
      afterNext &&
      !next.startsWith("<") &&
      isHtmlClosingTag(afterNext) &&
      htmlTagName(afterNext) === name
    ) {
      lines.push(indentLine(level, `${token}${next}${afterNext}`));
      i += 2;
      continue;
    }

    if (isHtmlClosingTag(token)) {
      level = Math.max(0, level - 1);
      lines.push(indentLine(level, token));
      continue;
    }

    lines.push(indentLine(level, token));
    if (!isHtmlSelfClosing(token)) level += 1;
  }

  return `${lines.join("\n")}\n`;
}

export function formatSource(language: SourceLanguage, source: string): string {
  if (language === "html") return formatHtmlSource(source);
  return `${source
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((line) => line.replace(/[ \t]+$/g, ""))
    .join("\n")
    .replace(/\n*$/g, "")}\n`;
}

export function sourceDiagnostics(language: SourceLanguage, source: string): Diagnostic[] {
  if (language === "plain") return [];
  const diagnostics: Diagnostic[] = [];
  // T4-10: track each line's start as a running offset instead of re-joining `lines.slice(0, index)`
  // per flagged line (was O(n^2) on whitespace-heavy files). `offset` after processing a line equals
  // exactly what the old `prior.length + (index === 0 ? 0 : 1)` computed for the next line — each line
  // contributes its own length plus one for the "\n" that joined it to the next.
  let offset = 0;
  for (const line of source.replace(/\r\n?/g, "\n").split("\n")) {
    if (/[ \t]+$/.test(line)) {
      diagnostics.push({
        from: offset + line.search(/[ \t]+$/),
        to: offset + line.length,
        severity: "info",
        source: hostAppName(),
        message: "Trailing whitespace.",
      });
    }
    offset += line.length + 1;
  }

  if (language !== "html") return diagnostics.sort((a, b) => a.from - b.from || a.to - b.to);
  const stack: { name: string; from: number; to: number }[] = [];
  const tagPattern = /<!--[\s\S]*?-->|<![^>]*>|<\/?([a-zA-Z][\w:-]*)\b[^>]*>/g;
  while (true) {
    const match = tagPattern.exec(source);
    if (!match) break;
    const token = match[0];
    const name = htmlTagName(token);
    if (!name || isHtmlMetaToken(token) || token.startsWith("<!--") || isHtmlSelfClosing(token))
      continue;
    if (!isHtmlClosingTag(token)) {
      stack.push({ name, from: match.index, to: match.index + token.length });
      continue;
    }
    const open = stack.pop();
    if (!open) {
      diagnostics.push({
        from: match.index,
        to: match.index + token.length,
        severity: "warning",
        source: hostAppName(),
        message: `Closing </${name}> has no opening tag.`,
      });
    } else if (open.name !== name) {
      diagnostics.push({
        from: match.index,
        to: match.index + token.length,
        severity: "warning",
        source: hostAppName(),
        message: `Closing </${name}> does not match <${open.name}>.`,
      });
      stack.push(open);
    }
  }
  for (const open of stack.reverse()) {
    diagnostics.push({
      from: open.from,
      to: open.to,
      severity: "warning",
      source: hostAppName(),
      message: `<${open.name}> is not closed.`,
    });
  }
  return diagnostics.sort((a, b) => a.from - b.from || a.to - b.to);
}

/** The self-contained image token to insert for a pasted image. */
export function buildImageEmbed(language: SourceLanguage, dataUrl: string, alt = ""): string {
  return language === "html" ? `<img src="${dataUrl}" alt="${alt}" />` : `![${alt}](${dataUrl})`;
}
