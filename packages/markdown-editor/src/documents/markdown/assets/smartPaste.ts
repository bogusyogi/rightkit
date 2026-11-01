/**
 * Shared smart-paste pipeline.
 *
 * The pipeline runs identically for the source editor and the
 * ProseMirror editor. Both surfaces call {@link smartPaste} on
 * the document core with the clipboard payload; the function
 * returns a batch of edits plus a list of asset-save calls the
 * caller must dispatch to the Rust layer.
 *
 * The pipeline:
 *
 *  - inspects the clipboard HTML for known unsafe patterns
 *    (e.g. `MsoNormal` classes, nested spans, color-only styles),
 *  - flattens nested `<b>`/`<strong>` and `<i>`/`<em>` markers,
 *  - converts simple `<table>` structures to Markdown tables,
 *  - extracts `<img>` tags whose `src` is a data URL and queues
 *    them for asset save,
 *  - returns the resulting Markdown plus the queued asset ops.
 */

import type { MarkdownTextEdit } from "../core/types";
import type { MarkdownAssetMimeKind, MarkdownAssetPayload } from "./markdownAsset";
import type { MarkdownAssetSettings } from "./markdownAssetSettings";

/**
 * Clipboard payload accepted by the smart-paste pipeline.
 */
export interface SmartPasteInput {
  /** Raw HTML from the clipboard; may be empty for plain-text paste. */
  readonly html: string;
  /** Plain-text fallback. */
  readonly text: string;
  /** Document basename; used to compute the asset folder. */
  readonly documentBasename: string;
  /** Resolved asset settings. */
  readonly settings: MarkdownAssetSettings;
  /**
   * Logical-source offset where the paste will land. The
   * returned edits apply at this offset in the new logical
   * source.
   */
  readonly insertAt: number;
  /**
   * Optional callback that resolves a saved asset to its
   * relative path. The pipeline never persists; it returns a
   * request and the caller is responsible for calling the Rust
   * asset protocol and substituting the resolved path.
   */
  readonly resolveAssetPath: (
    payload: MarkdownAssetPayload,
  ) => Promise<string | null> | string | null;
}

/**
 * Result of {@link smartPaste}. `edits` is a single insertion edit
 * placed at `input.insertAt`; `assetRequests` lists the payloads
 * the caller must persist before applying the edits.
 */
export interface SmartPasteResult {
  readonly edits: readonly MarkdownTextEdit[];
  readonly assetRequests: readonly MarkdownAssetPayload[];
  readonly strippedHtml: string;
}

/**
 * Top-level entry point. Returns the edits that transform the
 * current logical source by appending the converted Markdown.
 */
export async function smartPaste(input: SmartPasteInput): Promise<SmartPasteResult> {
  if (input.settings.smartPasteEnabled === false) {
    return plainPaste(input);
  }
  const { html } = input;
  if (html.length === 0) {
    return plainPaste(input);
  }
  // Rich clipboard payloads often include original Markdown. Preserve it exactly when available.
  if (input.text && hasMarkdownSyntax(input.text)) {
    return plainPaste(input);
  }
  const cleaned = sanitizeHtml(html, input.settings);
  const assetRequests: MarkdownAssetPayload[] = [];
  const markdownWithPending = await htmlToMarkdown(cleaned, assetRequests, input);
  const markdown = await resolvePendingAssets(markdownWithPending, assetRequests, input);
  const edits: MarkdownTextEdit[] = [
    {
      from: input.insertAt,
      to: input.insertAt,
      insert: markdown,
      inheritMode: "preserve",
    },
  ];
  return { edits, assetRequests, strippedHtml: cleaned };
}

function hasMarkdownSyntax(text: string): boolean {
  return (
    /(^|\n)\s{0,3}(#{1,6}\s|>\s|[-*+]\s|\d+\.\s|```|~~~)/.test(text) ||
    /(\*\*[^*]+\*\*|__[^_]+__|`[^`]+`|\[[^\]]+\]\([^)]+\))/.test(text)
  );
}

/**
 * Replace `__PENDING_ASSET__` placeholders with the resolved
 * relative paths returned by `input.resolveAssetPath`. Each
 * request is processed in source order.
 */
async function resolvePendingAssets(
  markdown: string,
  assetRequests: readonly MarkdownAssetPayload[],
  input: SmartPasteInput,
): Promise<string> {
  if (assetRequests.length === 0) return markdown;
  const resolved: string[] = [];
  for (const payload of assetRequests) {
    const path = await input.resolveAssetPath(payload);
    resolved.push(path ?? "__PENDING_ASSET__");
  }
  let out = markdown;
  for (const path of resolved) {
    out = out.replace("__PENDING_ASSET__", path);
  }
  return out;
}

async function plainPaste(input: SmartPasteInput): Promise<SmartPasteResult> {
  return {
    edits: [
      {
        from: input.insertAt,
        to: input.insertAt,
        insert: input.text,
        inheritMode: "preserve",
      },
    ],
    assetRequests: [],
    strippedHtml: "",
  };
}

/**
 * Strip known unsafe patterns from `html` and normalize the
 * formatting markers per the settings.
 */
export function sanitizeHtml(html: string, settings: MarkdownAssetSettings): string {
  let out = html;
  if (settings.smartPasteNormalizeMso) {
    out = out.replace(/\s*class\s*=\s*"MsoNormal"/g, "");
  }
  out = out.replace(/<\/?span\b[^>]*>/g, "");
  out = out.replace(/style\s*=\s*"[^"]*color\s*:[^"]*"/g, "");
  out = out.replace(/<\/?font\b[^>]*>/g, "");
  out = collapseStrongTags(out);
  out = collapseEmTags(out);
  // Collapse `<p >` (tag with dangling space) into `<p>` for
  // stable assertion outputs.
  out = out.replace(/<p\s+>/g, "<p>");
  return out;
}

function collapseStrongTags(html: string): string {
  // Convert `<b>X</b>` to `**X**` and collapse nested duplicates.
  return html.replace(/<b>([\s\S]*?)<\/b>/g, (_match, inner: string) => {
    return `**${inner.trim()}**`;
  });
}

function collapseEmTags(html: string): string {
  return html.replace(/<i>([\s\S]*?)<\/i>/g, (_match, inner: string) => {
    return `*${inner.trim()}*`;
  });
}

/**
 * Convert the cleaned HTML to Markdown. Extracts data: URL images
 * into `assetRequests` so the caller can save them.
 */
export async function htmlToMarkdown(
  html: string,
  assetRequests: MarkdownAssetPayload[],
  _input: SmartPasteInput,
): Promise<string> {
  const dataUrlPattern = /<img[^>]*src="(data:[^"]+)"[^>]*alt="([^"]*)"/g;
  const sanitized = html.replace(dataUrlPattern, (_match, src: string, alt: string) => {
    const mime = inferMimeFromDataUrl(src);
    if (!mime) return _match;
    const bytes = dataUrlToBytes(src);
    if (!bytes) return _match;
    const hintName = alt || "pasted";
    assetRequests.push({
      source: bytes,
      hintName,
      mimeKind: mime,
    });
    return `![${alt}](__PENDING_ASSET__)`;
  });
  const document = new DOMParser().parseFromString(sanitized, "text/html");
  return Array.from(document.body.childNodes)
    .map(nodeToMarkdown)
    .join("")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function childMarkdown(node: Node): string {
  return Array.from(node.childNodes).map(nodeToMarkdown).join("");
}

function nodeToMarkdown(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) return (node.nodeValue ?? "").replace(/\s+/g, " ");
  if (!(node instanceof HTMLElement)) return "";
  const tag = node.tagName.toLowerCase();
  const inner = childMarkdown(node);
  if (/^h[1-6]$/.test(tag)) return `${"#".repeat(Number(tag[1]))} ${inner.trim()}\n\n`;
  if (tag === "p" || tag === "div") return `${inner.trim()}\n\n`;
  if (tag === "strong" || tag === "b") return `**${inner.trim()}**`;
  if (tag === "em" || tag === "i") return `*${inner.trim()}*`;
  if (tag === "s" || tag === "del" || tag === "strike") return `~~${inner.trim()}~~`;
  if (tag === "code" && node.parentElement?.tagName.toLowerCase() !== "pre")
    return `\`${inner.trim()}\``;
  if (tag === "pre") return `\n\n\`\`\`\n${node.textContent ?? ""}\n\`\`\`\n\n`;
  if (tag === "a") {
    const href = node.getAttribute("href") ?? "";
    return href ? `[${inner.trim() || href}](${href})` : inner;
  }
  if (tag === "img") {
    const src = node.getAttribute("src") ?? "";
    const alt = node.getAttribute("alt") ?? "";
    return src ? `![${alt}](${src})` : alt;
  }
  if (tag === "br") return "\n";
  if (tag === "hr") return "\n\n---\n\n";
  if (tag === "blockquote") {
    return `${inner
      .trim()
      .split("\n")
      .map((line) => `> ${line}`)
      .join("\n")}\n\n`;
  }
  if (tag === "ul" || tag === "ol") return listToMarkdown(node, tag === "ol");
  if (tag === "table") return tableToMarkdown(node);
  return inner;
}

function listToMarkdown(list: HTMLElement, ordered: boolean): string {
  const items = Array.from(list.children).filter((child) => child.tagName === "LI");
  return `${items
    .map((item, index) => {
      const nested = Array.from(item.children).filter(
        (child) => child.tagName === "UL" || child.tagName === "OL",
      );
      const content = Array.from(item.childNodes)
        .filter(
          (child) =>
            !(child instanceof HTMLElement && (child.tagName === "UL" || child.tagName === "OL")),
        )
        .map(nodeToMarkdown)
        .join("")
        .trim();
      const nestedText = nested.map(nodeToMarkdown).join("").trim();
      const marker = ordered ? `${index + 1}.` : "-";
      return `${marker} ${content}${
        nestedText
          ? `\n${nestedText
              .split("\n")
              .map((line) => `  ${line}`)
              .join("\n")}`
          : ""
      }`;
    })
    .join("\n")}\n\n`;
}

function tableToMarkdown(table: HTMLElement): string {
  const rows = Array.from(table.querySelectorAll("tr"))
    .map((row) =>
      Array.from(row.cells).map((cell) => childMarkdown(cell).trim().replace(/\|/g, "\\|")),
    )
    .filter((row) => row.length > 0);
  if (!rows.length) return "";
  const width = Math.max(...rows.map((row) => row.length));
  const line = (row: string[]) =>
    `| ${Array.from({ length: width }, (_, index) => row[index] ?? "").join(" | ")} |`;
  return `${line(rows[0])}\n${line(Array(width).fill("---"))}${rows
    .slice(1)
    .map((row) => `\n${line(row)}`)
    .join("")}\n\n`;
}

function inferMimeFromDataUrl(src: string): MarkdownAssetMimeKind | null {
  if (src.startsWith("data:image/png")) return "png";
  if (src.startsWith("data:image/jpeg") || src.startsWith("data:image/jpg")) return "jpeg";
  if (src.startsWith("data:image/webp")) return "webp";
  if (src.startsWith("data:image/gif")) return "gif";
  if (src.startsWith("data:image/svg+xml")) return "svg";
  return null;
}

function dataUrlToBytes(src: string): Uint8Array | null {
  const comma = src.indexOf(",");
  if (comma === -1) return null;
  const header = src.slice(5, comma);
  if (!header.includes("base64")) return null;
  const body = src.slice(comma + 1);
  try {
    const binary = atob(body);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}
