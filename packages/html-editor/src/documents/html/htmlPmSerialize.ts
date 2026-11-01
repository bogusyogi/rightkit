import type { Mark as PmMark, Node as PmNode } from "prosemirror-model";
import { htmlTextStyleCss } from "./htmlTextStyle";

/**
 * Serialize a ProseMirror Visual doc back to HTML fragment (body inner).
 * For a full body patch we serialize doc.content; for single-node structural
 * edits the caller may pass a sub-node.
 * Canonical output is valid HTML, not byte-identical to input. Source outside
 * the serialized range belongs to the caller: `raw_html_island` nodes emit
 * their preserved raw source verbatim instead of being re-serialized.
 */

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function escapeAttr(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

const markOrder: Record<string, number> = {
  code: 0,
  strong: 10,
  em: 20,
  underline: 30,
  strike: 40,
  textStyle: 50,
  link: 100,
};

function orderedMarks(node: PmNode): PmMark[] {
  return [...node.marks]
    .filter((mark) => mark.type.name in markOrder)
    .sort((left, right) => markOrder[right.type.name] - markOrder[left.type.name]);
}

function openMark(mark: PmMark): string {
  switch (mark.type.name) {
    case "code":
      return "<code>";
    case "link": {
      const href = escapeAttr(mark.attrs.href ?? "");
      const title = mark.attrs.title ? ` title="${escapeAttr(mark.attrs.title)}"` : "";
      return `<a href="${href}"${title}>`;
    }
    case "strong":
      return "<strong>";
    case "em":
      return "<em>";
    case "underline":
      return "<u>";
    case "strike":
      return "<s>";
    case "textStyle": {
      const style = escapeAttr(htmlTextStyleCss(mark.attrs));
      return style ? `<span style="${style}">` : "";
    }
    default:
      return "";
  }
}

function closeMark(mark: PmMark): string {
  switch (mark.type.name) {
    case "code":
      return "</code>";
    case "link":
      return "</a>";
    case "strong":
      return "</strong>";
    case "em":
      return "</em>";
    case "underline":
      return "</u>";
    case "strike":
      return "</s>";
    case "textStyle":
      return htmlTextStyleCss(mark.attrs) ? "</span>" : "";
    default:
      return "";
  }
}

function serializeInlineContent(node: PmNode): string {
  let out = "";
  let activeMarks: PmMark[] = [];

  node.forEach((child) => {
    const nextMarks = orderedMarks(child);
    let shared = 0;
    while (
      shared < activeMarks.length &&
      shared < nextMarks.length &&
      activeMarks[shared].eq(nextMarks[shared])
    )
      shared++;

    for (let index = activeMarks.length - 1; index >= shared; index--)
      out += closeMark(activeMarks[index]);
    for (let index = shared; index < nextMarks.length; index++) out += openMark(nextMarks[index]);

    out += serializeNode(child, false);
    activeMarks = nextMarks;
  });

  for (let index = activeMarks.length - 1; index >= 0; index--)
    out += closeMark(activeMarks[index]);
  return out;
}

function serializeNode(node: PmNode, includeOwnMarks = true): string {
  if (includeOwnMarks && node.isInline && !node.isText && node.marks.length) {
    let html = serializeNode(node, false);
    const marks = orderedMarks(node);
    for (let index = marks.length - 1; index >= 0; index--)
      html = `${openMark(marks[index])}${html}${closeMark(marks[index])}`;
    return html;
  }
  if (node.isText) {
    let text = escapeHtml(node.text ?? "");
    if (!includeOwnMarks) return text;
    const marks = orderedMarks(node);
    for (let index = marks.length - 1; index >= 0; index--)
      text = `${openMark(marks[index])}${text}${closeMark(marks[index])}`;
    return text;
  }

  const type = node.type.name;
  switch (type) {
    case "doc": {
      if (node.inlineContent) return serializeInlineContent(node);
      let html = "";
      node.forEach((child) => {
        html += serializeNode(child);
      });
      return html;
    }
    case "paragraph": {
      return `<p>${serializeInlineContent(node)}</p>`;
    }
    case "heading": {
      const level = Math.max(1, Math.min(6, Number(node.attrs.level) || 1));
      return `<h${level}>${serializeInlineContent(node)}</h${level}>`;
    }
    case "blockquote": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<blockquote>${inner}</blockquote>`;
    }
    case "article": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<article>${inner}</article>`;
    }
    case "section": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<section>${inner}</section>`;
    }
    case "html_div": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<div>${inner}</div>`;
    }
    case "html_span": {
      return `<span>${serializeInlineContent(node)}</span>`;
    }
    case "bullet_list": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<ul>${inner}</ul>`;
    }
    case "ordered_list": {
      const start =
        node.attrs.start && node.attrs.start !== 1 ? ` start="${node.attrs.start}"` : "";
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<ol${start}>${inner}</ol>`;
    }
    case "list_item": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<li>${inner}</li>`;
    }
    case "code_block": {
      const text = escapeHtml(node.textContent);
      return `<pre><code>${text}</code></pre>`;
    }
    case "horizontal_rule": {
      return `<hr>`;
    }
    case "hard_break": {
      return `<br>`;
    }
    case "image": {
      const src = escapeAttr(node.attrs.src ?? "");
      const alt = escapeAttr(node.attrs.alt ?? "");
      const title = node.attrs.title ? ` title="${escapeAttr(node.attrs.title)}"` : "";
      const altAttr = ` alt="${alt}"`;
      return `<img src="${src}"${altAttr}${title}>`;
    }
    case "table": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<table>${inner}</table>`;
    }
    case "table_caption": {
      return `<caption>${serializeInlineContent(node)}</caption>`;
    }
    case "table_head": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<thead>${inner}</thead>`;
    }
    case "table_body": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<tbody>${inner}</tbody>`;
    }
    case "table_foot": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<tfoot>${inner}</tfoot>`;
    }
    case "table_row": {
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return `<tr>${inner}</tr>`;
    }
    case "table_cell": {
      const colspan =
        node.attrs.colspan && node.attrs.colspan !== 1 ? ` colspan="${node.attrs.colspan}"` : "";
      const rowspan =
        node.attrs.rowspan && node.attrs.rowspan !== 1 ? ` rowspan="${node.attrs.rowspan}"` : "";
      const align = node.attrs.align
        ? ` style="text-align:${escapeAttr(String(node.attrs.align))}"`
        : "";
      return `<td${colspan}${rowspan}${align}>${serializeInlineContent(node)}</td>`;
    }
    case "table_header": {
      const colspan =
        node.attrs.colspan && node.attrs.colspan !== 1 ? ` colspan="${node.attrs.colspan}"` : "";
      const rowspan =
        node.attrs.rowspan && node.attrs.rowspan !== 1 ? ` rowspan="${node.attrs.rowspan}"` : "";
      const align = node.attrs.align
        ? ` style="text-align:${escapeAttr(String(node.attrs.align))}"`
        : "";
      return `<th${colspan}${rowspan}${align}>${serializeInlineContent(node)}</th>`;
    }
    case "raw_html_island": {
      // Raw islands are not re-serialized from the Visual tree; emit the
      // preserved raw source verbatim.
      return String(node.attrs.raw ?? "");
    }
    default: {
      // Fallback: serialize children
      let inner = "";
      node.forEach((child) => {
        inner += serializeNode(child);
      });
      return inner;
    }
  }
}

export function serializePmToHtmlFragment(doc: PmNode): string {
  // If doc is a full doc node, serialize its content as fragment (body inner)
  if (doc.type.name === "doc") return serializeNode(doc);
  // If caller passes a single block node, serialize it
  return serializeNode(doc);
}
