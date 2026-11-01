import type { Mark, Node as ProseMirrorNode } from "prosemirror-model";
import type { ListItem, PhrasingContent, Root, RootContent, Table, Text } from "mdast";
import { parseToTree } from "../parseMarkdown.js";
import { sourceSliceForPosition } from "../pmMarkdownSerialize.js";
import { isCallout } from "../semanticBlocks.js";
import { markdownHybridSchema } from "../markdownHybridSchema.js";
import { parseGfmTable } from "../tables/markdownTableParser.js";
import { parseCodeFenceSource } from "../codeFenceModel.js";
import { incParseCount } from "@rightkit/document-core/documents/perfEventCounters";
import { normalizeSoftWraps } from "./pmToMarkdown.js";

type Mdastish = Root | RootContent;

/** Bounded LRU projection cache. Mounted renderers share recent source projections without
 * retaining every cold tab's full document tree. */
const pmDocCache = new Map<string, ReturnType<typeof markdownToPmDocUncached>>();
const PM_DOC_CACHE_MAX = 4;

function markdownToPmDocUncached(source: string) {
  let tree: Root;
  try {
    tree = parseToTree(source);
  } catch {
    return rawDocFromSource(source, "parse-error");
  }
  const blocks: ProseMirrorNode[] = [];
  let priorEnd = 0;
  for (const child of tree.children) {
    const start = child.position?.start.offset;
    const end = child.position?.end.offset;
    if (typeof start !== "number" || typeof end !== "number") {
      // Synthetic nodes (definition lists, abbreviation definitions) carry no source positions.
      blocks.push(mdastBlockToPm(source, child, ""));
      continue;
    }
    if (start < priorEnd || end < start) {
      return rawDocFromSource(source, "missing-position");
    }
    const block = mdastBlockToPm(source, child, source.slice(priorEnd, start));
    blocks.push(
      block.type.create(
        {
          ...block.attrs,
          sourceFrom: start,
          sourceTo: end,
          prefixFrom: priorEnd,
          prefixTo: start,
        },
        block.content,
        block.marks,
      ),
    );
    priorEnd = end;
  }
  if (!blocks.length) {
    blocks.push(markdownHybridSchema.nodes.paragraph.create({ rawSource: "", rawText: "" }));
  }
  return markdownHybridSchema.nodes.doc.create({ trailing: source.slice(priorEnd) }, blocks);
}

export function markdownToPmDoc(source: string) {
  const cached = pmDocCache.get(source);
  if (cached) {
    pmDocCache.delete(source);
    pmDocCache.set(source, cached);
    return cached;
  }
  incParseCount(1);
  const doc = markdownToPmDocUncached(source);
  pmDocCache.set(source, doc);
  if (pmDocCache.size > PM_DOC_CACHE_MAX) {
    const oldest = pmDocCache.keys().next().value;
    if (oldest !== undefined) pmDocCache.delete(oldest);
  }
  return doc;
}


function rawDocFromSource(source: string, reason: string) {
  return markdownHybridSchema.nodes.doc.create({ trailing: "" }, [
    markdownHybridSchema.nodes.raw_block.create({
      prefix: "",
      rawSource: source,
      mdastType: reason,
      sourceLine: 1,
    }),
  ]);
}

function mdastBlockToPm(source: string, node: RootContent, prefix: string): ProseMirrorNode {
  const rawSource = sourceSliceForPosition(source, node.position) ?? "";
  if (node.type === "heading") {
    const text = mdastText(node);
    const inlineContent = inlineContentFromMdast(node);
    if (
      !inlineContent ||
      (!isPlainHeadingSource(rawSource, node.depth, text) &&
        !rawSource.startsWith("#".repeat(node.depth)))
    ) {
      return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
    }
    return markdownHybridSchema.nodes.heading.create(
      { level: Math.min(Math.max(node.depth, 1), 6), prefix, rawSource, rawText: text },
      inlineContent.length ? inlineContent : null,
    );
  }
  if (node.type === "paragraph" && !hasInlineHtml(node)) {
    const text = mdastText(node);
    if (text.trim().toLowerCase() === "[toc]") {
      return rawBlock(prefix, rawSource, "toc", node.position?.start.line);
    }
    const inlineContent = inlineContentFromMdast(node);
    if (!inlineContent) {
      return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
    }
    return markdownHybridSchema.nodes.paragraph.create(
      { prefix, rawSource, rawText: text },
      inlineContent.length ? inlineContent : null,
    );
  }
  if (node.type === "blockquote") {
    // A GFM callout (`> [!NOTE] …`) has to take the raw_block path. raw_block is what
    // MarkdownProtectedBlock re-parses through the detectors and renders with SemanticBlock, so
    // it is the ONLY route to the Callout component. Converting it to an editable blockquote
    // instead — which is what every blockquote used to do — left the literal "[!NOTE]" marker
    // sitting in the rendered text, because nothing downstream ever strips it. This matches how
    // rich tables and steppers already reach their components: fall through to raw_block.
    if (isCallout(node)) {
      return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
    }
    const children = childBlocksToPm(source, node.children);
    if (!children) return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
    return markdownHybridSchema.nodes.blockquote.create(
      { prefix, rawSource, rawText: mdastText(node) },
      children,
    );
  }
  if (node.type === "list" && !node.children.some((item) => typeof item.checked === "boolean")) {
    const items = node.children.map((item) => listItemToPm(source, item));
    if (items.some((item) => !item))
      return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
    const type = node.ordered
      ? markdownHybridSchema.nodes.ordered_list
      : markdownHybridSchema.nodes.bullet_list;
    return type.create(
      { prefix, rawSource, rawText: mdastText(node), start: node.start ?? 1 },
      items as ProseMirrorNode[],
    );
  }
  if ((node.type as string) === "descriptionList") {
    const list = node as unknown as {
      children: { terms: { children: RootContent[] }[]; details: { children: RootContent[] }[] }[];
    };
    const items = list.children.map((item) => {
      const terms = item.terms
        .map((t) =>
          inlineContentFromMdast({ type: "paragraph", children: t.children } as RootContent),
        )
        .flatMap((content) =>
          content ? [markdownHybridSchema.nodes.description_term.create(null, content)] : [],
        );
      const details = item.details
        .map((d) =>
          inlineContentFromMdast({ type: "paragraph", children: d.children } as RootContent),
        )
        .flatMap((content) =>
          content ? [markdownHybridSchema.nodes.description_details.create(null, content)] : [],
        );
      if (!terms.length || !details.length) return null;
      return markdownHybridSchema.nodes.description_item.create(null, [...terms, ...details]);
    });
    if (items.some((item) => !item))
      return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
    return markdownHybridSchema.nodes.description_list.create(
      { prefix, rawSource, rawText: mdastText(node) },
      items as ProseMirrorNode[],
    );
  }
  if ((node.type as string) === "abbreviationDefinition") {
    const abbrNode = node as unknown as {
      short: string;
      long: string;
      position?: { start: { line: number } };
    };
    return rawBlock(
      prefix,
      `*[${abbrNode.short}]: ${abbrNode.long}`,
      "abbreviationDefinition",
      abbrNode.position?.start.line,
    );
  }
  if ((node.type as string) === "bibliography") {
    return rawBlock(prefix, "[bibliography]", "bibliography", node.position?.start.line);
  }
  if (node.type === "table") {
    const pmTable = mdastTableToPm(node as Table, prefix, rawSource);
    if (pmTable) return pmTable;
    return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
  }
  if (node.type === "code") {
    const codeFence = mdastCodeToPm(prefix, rawSource, node);
    if (codeFence) return codeFence;
    return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
  }
  return rawBlock(prefix, rawSource, node.type, node.position?.start.line);
}

function childBlocksToPm(source: string, children: RootContent[]): ProseMirrorNode[] | null {
  const blocks: ProseMirrorNode[] = children.map((child) => mdastBlockToPm(source, child, ""));
  return blocks.some((block) => block.type.name === "raw_block") ? null : blocks;
}

function listItemToPm(source: string, item: ListItem): ProseMirrorNode | null {
  if (typeof item.checked === "boolean") return null;
  const children = childBlocksToPm(source, item.children as RootContent[]);
  if (!children?.length) return null;
  return markdownHybridSchema.nodes.list_item.create(
    { rawText: mdastText(item as RootContent) },
    children,
  );
}

function mdastTableToPm(node: Table, prefix: string, rawSource: string): ProseMirrorNode | null {
  const parsed = parseGfmTable(node, {
    schema: markdownHybridSchema,
    source: { prefix, rawSource },
    parseInline: (children) => {
      const content: ProseMirrorNode[] = [];
      for (const child of children) {
        const type = (child as { type?: string }).type;
        if (type === "html") {
          const value = (child as unknown as { value?: string }).value ?? "";
          if (/^<\s*br\s*\/?\s*>$/i.test(value.trim())) {
            content.push(markdownHybridSchema.text("\n"));
            continue;
          }
          return null;
        }
        const childContent = inlineContentFromMdast(child as PhrasingContent, []);
        if (childContent === null) return null;
        content.push(...childContent);
      }
      return content;
    },
  });
  return parsed.kind === "editable" ? parsed.node : null;
}

function rawBlock(prefix: string, rawSource: string, mdastType: string, sourceLine = 1) {
  return markdownHybridSchema.nodes.raw_block.create({
    prefix,
    rawSource,
    mdastType,
    sourceLine,
  });
}

/**
 * Project an mdast `code` node to the native `code_fence` node when the raw
 * source is a representable fenced block (backtick/tilde fence with a
 * recoverable info string). Returns `null` for indented code and malformed
 * fences so the caller keeps the raw/protected fallback path.
 */
function mdastCodeToPm(
  prefix: string,
  rawSource: string,
  node: RootContent,
): ProseMirrorNode | null {
  const parsed = parseCodeFenceSource(rawSource);
  if (!parsed) return null;
  const code = node as unknown as { lang?: string | null; meta?: string | null; value?: string };
  const language = code.lang ?? parsed.language ?? "";
  // Mermaid (and its mmd alias) renders as a diagram, not editable source. It must keep
  // the raw/protected path so `RenderedProtectedMarkdownBlock` → `protectedKindForSource`
  // classifies it as a mermaid block. Same for any construct whose rendering is semantic.
  if (language === "mermaid" || language === "mmd") return null;
  const content = code.value ?? parsed.content;
  // mdast's `code.value` removes indentation belonging to an indented fence,
  // while `rawSource` keeps it. Derive that lossless mapping from parsed bytes
  // and projected content; `prefix` is outside rawSource and is not content.
  const contentIndent = inferCodeContentIndent(parsed.content, content, parsed.fenceIndent);
  return markdownHybridSchema.nodes.code_fence.create(
    {
      prefix,
      rawSource,
      rawText: content,
      language,
      meta: code.meta ?? parsed.meta ?? "",
      fenceChar: parsed.fenceChar,
      fenceLength: parsed.fenceLength,
      fenceIndent: parsed.fenceIndent,
      contentIndent,
      openingSuffix: parsed.openingSuffix,
      trailingNewline: parsed.trailingNewline,
      infoFrom: parsed.infoFrom,
      infoTo: parsed.infoTo,
      contentFrom: parsed.contentFrom,
      contentTo: parsed.contentTo,
    },
    content ? [markdownHybridSchema.text(content)] : null,
  );
}

function inferCodeContentIndent(
  rawContent: string,
  projectedContent: string,
  fenceIndent: string,
): string {
  const rawLines = rawContent.split("\n");
  const projectedLines = projectedContent.split("\n");
  if (rawLines.length !== projectedLines.length) return fenceIndent;

  let inferred: string | null = fenceIndent || null;
  for (let index = 0; index < rawLines.length; index++) {
    const rawLine = rawLines[index] ?? "";
    const projectedLine = projectedLines[index] ?? "";
    if (!rawLine && !projectedLine) continue;
    if (rawLine === projectedLine) {
      if (inferred) return "";
      continue;
    }
    if (!rawLine.endsWith(projectedLine)) return "";
    const candidate = rawLine.slice(0, rawLine.length - projectedLine.length);
    if (!candidate || !/^[ \t]+$/.test(candidate)) return "";
    if (inferred === null) inferred = candidate;
    if (inferred !== candidate) return "";
  }
  return inferred ?? "";
}

function isPlainHeadingSource(rawSource: string, depth: number, text: string) {
  const marker = "#".repeat(Math.min(Math.max(depth, 1), 6));
  return rawSource === (text ? `${marker} ${text}` : marker);
}

function inlineContentFromMdast(
  node: Mdastish | PhrasingContent,
  marks: Mark[] = [],
): ProseMirrorNode[] | null {
  if (node.type === "text") {
    const value = normalizeSoftWraps(node.value);
    return value ? [markdownHybridSchema.text(value, marks)] : [];
  }
  if ((node as { type?: string }).type === "abbr") {
    const abbr = node as { title: string; children: Text[] };
    return inlineChildren(
      { type: "paragraph", children: abbr.children } as RootContent,
      withMark(marks, "abbr", { title: abbr.title }),
    );
  }
  if (node.type === "inlineCode") {
    return node.value
      ? [markdownHybridSchema.text(node.value, [markdownHybridSchema.marks.code.create()])]
      : [];
  }
  if (node.type === "break") {
    return [markdownHybridSchema.text("\n", marks)];
  }
  if (node.type === "strong") return inlineChildren(node, withMark(marks, "strong"));
  if (node.type === "emphasis") return inlineChildren(node, withMark(marks, "em"));
  if (node.type === "delete") return inlineChildren(node, withMark(marks, "delete"));
  if (["mark", "highlight"].includes((node as unknown as { type: string }).type))
    return inlineChildren(node, withMark(marks, "mark"));
  if (node.type === "link") {
    return inlineChildren(
      node,
      withMark(marks, "link", {
        href: node.url,
        title: node.title ?? null,
      }),
    );
  }
  if (node.type === "image") {
    return [
      markdownHybridSchema.nodes.image.create({
        src: node.url,
        alt: node.alt ?? "",
        title: node.title ?? null,
      }),
    ];
  }
  if ((node as { type?: string }).type === "wikiLink") {
    const wiki = node as unknown as { target: string; hash?: string; display?: string | null };
    return [
      markdownHybridSchema.nodes.wiki_link.create({
        target: wiki.target,
        hash: wiki.hash ?? "",
        display: wiki.display ?? "",
      }),
    ];
  }
  if ((node as { type?: string }).type === "citation") {
    const citation = node as unknown as { key: string; locator?: string };
    return [
      markdownHybridSchema.nodes.citation.create({
        key: citation.key,
        locator: citation.locator ?? "",
      }),
    ];
  }
  if (node.type === "paragraph" || node.type === "heading") return inlineChildren(node, marks);
  return null;
}

function inlineChildren(node: Mdastish | PhrasingContent, marks: Mark[]) {
  if (!("children" in node) || !Array.isArray(node.children)) return null;
  const content: ProseMirrorNode[] = [];
  for (const child of node.children) {
    const childContent = inlineContentFromMdast(child, marks);
    if (!childContent) return null;
    content.push(...childContent);
  }
  return content;
}

function withMark(
  marks: Mark[],
  name: "strong" | "em" | "code" | "delete" | "mark" | "link" | "abbr",
  attrs?: Record<string, unknown>,
) {
  const type = markdownHybridSchema.marks[name];
  return [...marks.filter((mark) => mark.type !== type), type.create(attrs)];
}

function hasInlineHtml(node: Mdastish): boolean {
  return "children" in node && Array.isArray(node.children)
    ? node.children.some((child) => child.type === "html" || hasInlineHtml(child))
    : false;
}

function mdastText(node: Mdastish): string {
  if ("value" in node && typeof node.value === "string") return normalizeSoftWraps(node.value);
  if (node.type === "break") return "\n";
  if (node.type === "footnoteReference") return `[^${node.identifier}]`;
  if (node.type === "image") return node.alt ?? "";
  if ("children" in node && Array.isArray(node.children))
    return node.children.map(mdastText).join("");
  return "";
}
