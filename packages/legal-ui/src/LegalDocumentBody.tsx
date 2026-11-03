import { Fragment, type ReactElement, type ReactNode } from "react";

export type LegalContentFormat = "markdown" | "text";

/** `legacy` (default) reproduces the 0.3.0 Markdown rendering byte for byte. `extended` strips HTML
 *  comments, keeps indented list continuations inside their item, and honours two-space hard breaks. */
export type LegalMarkdownMode = "legacy" | "extended";

const SAFE_HREF = /^(https?:\/\/|mailto:)/i;

/** A placeholder highlighter: every match of `pattern` in plain text is passed to `render`. */
interface Highlighter {
  pattern: RegExp;
  render: (match: string, index: number) => ReactNode;
}

function highlighterFor(pattern: RegExp | undefined, render: LegalDocumentBodyProps["renderHighlight"]): Highlighter | undefined {
  if (!pattern) return undefined;
  return {
    pattern,
    render: render ?? ((match) => <mark className="rk-legal-gate__placeholder">{match}</mark>),
  };
}

/** Splits plain text around highlighter matches; text with no match (or no highlighter) is returned as is. */
function highlightText(text: string, highlighter: Highlighter | undefined, key: string): ReactNode[] {
  if (!highlighter || text === "") return [text];
  const flags = highlighter.pattern.flags.includes("g") ? highlighter.pattern.flags : `${highlighter.pattern.flags}g`;
  const re = new RegExp(highlighter.pattern.source, flags);
  const out: ReactNode[] = [];
  let last = 0;
  let index = 0;
  for (let m = re.exec(text); m !== null; m = re.exec(text)) {
    if (m[0].length === 0) { re.lastIndex += 1; continue; }
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(<Fragment key={`${key}-hl${index}`}>{highlighter.render(m[0], index)}</Fragment>);
    index += 1;
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out.length ? out : [text];
}

/** Inline markdown: `code`, **bold**, *italic*, [text](url). Only http(s) and mailto links
 *  render as links; everything is built from React elements, never raw HTML. */
function renderInline(text: string, highlighter: Highlighter | undefined): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /`([^`]+)`|\*\*([^*]+)\*\*|\*([^*]+)\*|\[([^\]]+)\]\(([^)\s]+)\)/g;
  let last = 0;
  let key = 0;
  for (let m = re.exec(text); m !== null; m = re.exec(text)) {
    if (m.index > last) out.push(...highlightText(text.slice(last, m.index), highlighter, `t${key}`));
    if (m[1] !== undefined) out.push(<code key={key++}>{m[1]}</code>);
    else if (m[2] !== undefined) out.push(<strong key={key++}>{m[2]}</strong>);
    else if (m[3] !== undefined) out.push(<em key={key++}>{m[3]}</em>);
    else if (m[4] !== undefined && m[5] !== undefined) {
      out.push(
        SAFE_HREF.test(m[5]) ? (
          <a key={key++} href={m[5]} target="_blank" rel="noreferrer">{m[4]}</a>
        ) : (
          m[4]
        ),
      );
    }
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(...highlightText(text.slice(last), highlighter, `t${key}`));
  return out;
}

/** Extended paragraph: soft breaks join with a space so emphasis can span lines; a line ending in two
 *  spaces starts a new group after a `<br />`. Inline parsing runs per group, never per source line. */
function renderExtendedParagraph(lines: string[], highlighter: Highlighter | undefined): ReactNode[] {
  const groups: string[][] = [[]];
  lines.forEach((raw, n) => {
    groups[groups.length - 1]!.push(raw.trim());
    if (n < lines.length - 1 && raw.endsWith("  ")) groups.push([]);
  });
  return groups.flatMap((group, n) => [
    n > 0 ? <br key={`br${n}`} /> : null,
    <Fragment key={`g${n}`}>{renderInline(group.join(" "), highlighter)}</Fragment>,
  ]);
}

function renderMarkdown(source: string, mode: LegalMarkdownMode, title: string | undefined, highlighter: Highlighter | undefined): ReactElement[] {
  const extended = mode === "extended";
  const text = extended ? source.replace(/<!--[\s\S]*?-->/g, "") : source;
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const blocks: ReactElement[] = [];
  let key = 0;
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (line.trim() === "") { i++; continue; }
    const heading = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (heading) {
      if (blocks.length === 0 && heading[2]!.trim() === title) { i++; continue; }
      const level = Math.min(heading[1]!.length + 1, 6); // page owns h1
      const Tag = `h${level}` as "h2";
      blocks.push(<Tag key={key++}>{renderInline(heading[2]!, highlighter)}</Tag>);
      i++;
      continue;
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) { blocks.push(<hr key={key++} />); i++; continue; }
    const listMatch = /^\s*(?:([-*+])|(\d+)[.)])\s+/.exec(line);
    if (listMatch) {
      const ordered = listMatch[2] !== undefined;
      const items: string[] = [];
      while (i < lines.length) {
        const m = /^\s*(?:[-*+]|\d+[.)])\s+(.*)$/.exec(lines[i] ?? "");
        if (!m) {
          if (extended && items.length && /^\s{2,}\S/.test(lines[i] ?? "")) {
            items[items.length - 1] += ` ${(lines[i] ?? "").trim()}`;
            i++;
            continue;
          }
          break;
        }
        items.push(m[1]!);
        i++;
      }
      const children = items.map((item, n) => <li key={n}>{renderInline(item, highlighter)}</li>);
      blocks.push(ordered ? <ol key={key++}>{children}</ol> : <ul key={key++}>{children}</ul>);
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && (lines[i] ?? "").trim() !== "" && !/^(#{1,6}\s|\s*(?:[-*+]|\d+[.)])\s)/.test(lines[i] ?? "")) {
      para.push(lines[i] ?? "");
      i++;
    }
    blocks.push(<p key={key++}>{extended
      ? renderExtendedParagraph(para, highlighter)
      : renderInline(para.map((l) => l.trim()).join(" "), highlighter)}</p>);
  }
  return blocks;
}

export interface LegalDocumentBodyProps {
  content: string;
  format: LegalContentFormat;
  /** Omit matching leading Markdown headings when host already shows this title. */
  title?: string;
  /** Markdown dialect. Default `"legacy"` keeps 0.3.0 output; `"extended"` enables comment stripping,
   *  list continuations and hard breaks. Ignored for `format: "text"`. */
  markdown?: LegalMarkdownMode;
  /** Plain-text matches to highlight, e.g. `/\[\[FILL:[^\]]*\]\]/g` for GenRight blanks. A `g` flag is added when missing.
   *  Matches inside code, emphasis or link text are not highlighted. Absent: no highlighting. */
  highlightPattern?: RegExp;
  /** Renders one highlighted match. Default: `<mark class="rk-legal-gate__placeholder">` with the matched text. */
  renderHighlight?: (match: string, index: number) => ReactNode;
}

/** Renders bundled legal text. `text` keeps line breaks; `markdown` supports headings, paragraphs,
 *  lists, rules, and inline emphasis/code/links. No HTML is ever injected. */
export function LegalDocumentBody({ content, format, title, markdown = "legacy", highlightPattern, renderHighlight }: LegalDocumentBodyProps): ReactElement {
  const highlighter = highlighterFor(highlightPattern, renderHighlight);
  if (format === "text") {
    return <pre className="rk-legal-gate__doc-text">{highlightText(content, highlighter, "text")}</pre>;
  }
  return <Fragment>{renderMarkdown(content, markdown, title, highlighter)}</Fragment>;
}
