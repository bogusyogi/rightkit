import { Fragment, type ReactElement, type ReactNode } from "react";

export type LegalContentFormat = "markdown" | "text";

const SAFE_HREF = /^(https?:\/\/|mailto:)/i;

/** Inline markdown: `code`, **bold**, *italic*, [text](url). Only http(s) and mailto links
 *  render as links; everything is built from React elements, never raw HTML. */
function renderInline(text: string): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /`([^`]+)`|\*\*([^*]+)\*\*|\*([^*]+)\*|\[([^\]]+)\]\(([^)\s]+)\)/g;
  let last = 0;
  let key = 0;
  for (let m = re.exec(text); m !== null; m = re.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
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
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function renderMarkdown(source: string): ReactElement[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: ReactElement[] = [];
  let key = 0;
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (line.trim() === "") { i++; continue; }
    const heading = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (heading) {
      const level = Math.min(heading[1]!.length + 1, 6); // page owns h1
      const Tag = `h${level}` as "h2";
      blocks.push(<Tag key={key++}>{renderInline(heading[2]!)}</Tag>);
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
        if (!m) break;
        items.push(m[1]!);
        i++;
      }
      const children = items.map((item, n) => <li key={n}>{renderInline(item)}</li>);
      blocks.push(ordered ? <ol key={key++}>{children}</ol> : <ul key={key++}>{children}</ul>);
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && (lines[i] ?? "").trim() !== "" && !/^(#{1,6}\s|\s*(?:[-*+]|\d+[.)])\s)/.test(lines[i] ?? "")) {
      para.push((lines[i] ?? "").trim());
      i++;
    }
    blocks.push(<p key={key++}>{renderInline(para.join(" "))}</p>);
  }
  return blocks;
}

/** Renders bundled legal text. `text` keeps line breaks; `markdown` supports headings, paragraphs,
 *  lists, rules, and inline emphasis/code/links. No HTML is ever injected. */
export function LegalDocumentBody({ content, format }: { content: string; format: LegalContentFormat }): ReactElement {
  if (format === "text") {
    return <pre className="rk-legal-gate__doc-text">{content}</pre>;
  }
  return <Fragment>{renderMarkdown(content)}</Fragment>;
}
