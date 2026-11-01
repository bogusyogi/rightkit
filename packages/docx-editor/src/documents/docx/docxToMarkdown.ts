import type { Document, Paragraph, Run, Table } from "@eigenpal/docx-editor-core";

type Inline = Paragraph["content"][number];

function runText(run: Run): string {
  let out = "";
  for (const c of run.content) {
    if (c.type === "text") out += c.text;
    else if (c.type === "tab") out += " ";
    else if (c.type === "break" && c.breakType !== "page") out += " ";
  }
  return out;
}

function styledRun(run: Run): string {
  const text = runText(run);
  if (!text.trim()) return text;
  const lead = text.match(/^\s*/)?.[0] ?? "";
  const trail = text.match(/\s*$/)?.[0] ?? "";
  let core = text.trim().replace(/([\\*_`[\]|])/g, "\\$1");
  const f = run.formatting;
  if (f?.bold && f?.italic) core = `***${core}***`;
  else if (f?.bold) core = `**${core}**`;
  else if (f?.italic) core = `*${core}*`;
  if (f?.strike) core = `~~${core}~~`;
  return lead + core + trail;
}

function inline(items: readonly Inline[]): string {
  let out = "";
  for (const item of items) {
    if (item.type === "run") out += styledRun(item);
    else if (item.type === "hyperlink") {
      const label = item.children.map((c) => (c.type === "run" ? styledRun(c) : "")).join("");
      out += item.href && label.trim() ? `[${label.trim()}](${item.href})` : label;
    } else if (item.type === "insertion") {
      out += inline((item as unknown as { content: Inline[] }).content ?? []);
    }
  }
  return out.replace(/[ \t]+/g, " ").trim();
}

function paragraph(p: Paragraph): string {
  const text = inline(p.content);
  if (!text) return "";
  const style = p.formatting?.styleId ?? "";
  const heading = /^(?:heading|titre|überschrift)\s*([1-6])$/i.exec(style);
  if (heading) return `${"#".repeat(Number(heading[1]))} ${text}`;
  if (/^title$/i.test(style)) return `# ${text}`;
  if (/^subtitle$/i.test(style)) return `## ${text}`;
  const list = p.listRendering;
  if (list && !list.markerHidden) {
    const indent = "  ".repeat(Math.max(0, list.level));
    return `${indent}${list.isBullet ? "-" : "1."} ${text}`;
  }
  return text;
}

function table(t: Table): string {
  const rows = t.rows.map((row) =>
    row.cells.map((cell) =>
      cell.content
        .map((b) => (b.type === "paragraph" ? paragraph(b).replace(/^#+\s+/, "") : ""))
        .filter(Boolean)
        .join("<br>")
        .replace(/\|/g, "\\|"),
    ),
  );
  if (!rows.length) return "";
  const width = Math.max(...rows.map((r) => r.length), 1);
  const pad = (r: string[]) => `| ${[...r, ...Array(width - r.length).fill("")].join(" | ")} |`;
  return [pad(rows[0]), `| ${Array(width).fill("---").join(" | ")} |`, ...rows.slice(1).map(pad)].join("\n");
}

/** Convert a parsed DOCX model (headings, lists, tables, bold/italic, links) to Markdown. */
export function docxModelToMarkdown(doc: Document): string {
  const blocks: string[] = [];
  let prevList = false;
  for (const block of doc.package.document.content) {
    if (block.type === "paragraph") {
      const md = paragraph(block);
      if (!md) continue;
      const isList = !!block.listRendering && !block.listRendering.markerHidden;
      if (isList && prevList) blocks[blocks.length - 1] += `\n${md}`;
      else blocks.push(md);
      prevList = isList;
    } else if (block.type === "table") {
      const md = table(block);
      if (md) blocks.push(md);
      prevList = false;
    }
  }
  return `${blocks.join("\n\n")}\n`;
}

export async function docxBytesToMarkdown(buffer: ArrayBuffer): Promise<string> {
  const { parseDocx } = await import("@eigenpal/docx-editor-core");
  return docxModelToMarkdown(await parseDocx(buffer, { preloadFonts: false }));
}
