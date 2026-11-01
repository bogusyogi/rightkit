import * as pdfjs from "pdfjs-dist";

interface Item {
  str: string;
  hasEOL?: boolean;
  height?: number;
  transform?: number[];
}

/** One page's text as paragraphs: lines join on EOL/baseline change; a vertical gap over 1.6 line
 *  heights starts a new paragraph. Plain paragraphs only (no font-size heading inference). */
export function pageItemsToParagraphs(items: Item[]): string[] {
  const paras: string[] = [];
  let line = "";
  let cur = "";
  let lastY: number | null = null;
  let lastH = 0;
  const flushLine = () => {
    const t = line.replace(/\s+/g, " ").trim();
    if (t) cur += (cur ? " " : "") + t;
    line = "";
  };
  const flushPara = () => {
    flushLine();
    if (cur) paras.push(cur);
    cur = "";
  };
  for (const it of items) {
    const y = it.transform?.[5] ?? null;
    const h = it.height || lastH || 10;
    if (y !== null && lastY !== null && Math.abs(y - lastY) > h * 0.5) {
      const gap = Math.abs(y - lastY);
      flushLine();
      if (gap > h * 1.6) flushPara();
    }
    if (y !== null) lastY = y;
    lastH = h;
    line += it.str;
    if (it.hasEOL) flushLine();
  }
  flushPara();
  return paras;
}

export async function pdfUrlToMarkdown(url: string): Promise<string> {
  const task = pdfjs.getDocument({ url });
  const doc = await task.promise;
  try {
    const pages: string[] = [];
    for (let n = 1; n <= doc.numPages; n++) {
      const page = await doc.getPage(n);
      const reader = page.streamTextContent().getReader();
      const items: Item[] = [];
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        for (const i of value.items) if ("str" in i) items.push(i as Item);
      }
      const paras = pageItemsToParagraphs(items);
      if (paras.length) pages.push(paras.join("\n\n"));
    }
    return `${pages.join("\n\n")}\n`;
  } finally {
    void task.destroy();
  }
}
