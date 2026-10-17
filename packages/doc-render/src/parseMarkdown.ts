import type { Root } from "mdast";
import { unified } from "unified";
import remarkFrontmatter from "remark-frontmatter";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import remarkParse from "remark-parse";
import remarkSmartypants from "remark-smartypants";
import { buildBlocks, type Block } from "./detectors/index.js";
import { remarkCitations } from "./remarkCitations.js";
import { remarkDefListAbbr } from "./remarkDefListAbbr.js";
import { remarkWikiLink } from "./remarkWikiLink.js";
import { remarkAutolinkFlag, remarkHighlight, remarkPreserveSoftBreaks } from "./remarkPhrasing.js";

/** Every flag defaults to the historical behavior except `highlight` and `softBreaks`, which are opt-in. */
export interface ParseOptions {
  smartypants?: boolean;
  frontmatter?: boolean;
  citations?: boolean;
  wikiLinks?: boolean;
  defLists?: boolean;
  /** `"preserve"` renders single newlines in a paragraph as line breaks. */
  softBreaks?: "preserve" | "collapse";
  /** `false` turns bare email autolinks into plain text; link nodes from bare autolinks carry `data.autolink`. */
  emailAutolinks?: boolean;
  /** Parse `==text==` into `highlight` nodes. Off by default. */
  highlight?: boolean;
}

function buildProcessor(options: ParseOptions) {
  const processor = unified().use(remarkParse).use(remarkGfm);
  if (options.frontmatter !== false) processor.use(remarkFrontmatter, ["yaml"]);
  processor.use(remarkMath);
  if (options.wikiLinks !== false) processor.use(remarkWikiLink);
  if (options.defLists !== false) processor.use(remarkDefListAbbr);
  if (options.citations !== false) processor.use(remarkCitations);
  if (options.highlight === true) processor.use(remarkHighlight);
  processor.use(remarkAutolinkFlag, { keepEmail: options.emailAutolinks !== false });
  if (options.softBreaks === "preserve") processor.use(remarkPreserveSoftBreaks);
  if (options.smartypants !== false) processor.use(remarkSmartypants);
  return processor;
}

function optionsKey(options: ParseOptions): string {
  return [options.smartypants !== false, options.frontmatter !== false, options.citations !== false, options.wikiLinks !== false, options.defLists !== false, options.softBreaks === "preserve", options.emailAutolinks !== false, options.highlight === true].map(Number).join("");
}

const processors = new Map<string, ReturnType<typeof buildProcessor>>();

interface CacheEntry { tree: Root; bytes: number }
const cache = new Map<string, CacheEntry>();
const MAX_CACHE_ENTRIES = 64;
export const DEFAULT_PARSE_CACHE_MAX_BYTES = 8 * 1024 * 1024;
let maxCacheBytes = DEFAULT_PARSE_CACHE_MAX_BYTES;
let cacheBytes = 0;

/** Rough retained size: UTF-16 source plus a flat per-node cost. Deliberately an estimate. */
function estimateBytes(key: string, tree: Root): number {
  let nodes = 0;
  const stack: unknown[] = [tree];
  while (stack.length) {
    const node = stack.pop() as { children?: unknown[] };
    nodes += 1;
    if (node.children) stack.push(...node.children);
  }
  return key.length * 2 + nodes * 160;
}

function evict(): void {
  while (cache.size && (cache.size > MAX_CACHE_ENTRIES || cacheBytes > maxCacheBytes)) {
    const oldest = cache.keys().next().value as string;
    cacheBytes -= cache.get(oldest)!.bytes;
    cache.delete(oldest);
  }
}

export function parseToTree(source: string, options: ParseOptions = {}): Root {
  const optKey = optionsKey(options);
  const key = `${optKey}\u0000${source}`;
  const cached = cache.get(key);
  if (cached) {
    cache.delete(key);
    cache.set(key, cached);
    return cached.tree;
  }
  let processor = processors.get(optKey);
  if (!processor) { processor = buildProcessor(options); processors.set(optKey, processor); }
  const tree = processor.runSync(processor.parse(source), { value: source } as never) as Root;
  const bytes = estimateBytes(key, tree);
  if (bytes <= maxCacheBytes) {
    cache.set(key, { tree, bytes });
    cacheBytes += bytes;
    evict();
  }
  return tree;
}

export function parseMarkdown(source: string, options?: ParseOptions): Block[] {
  return buildBlocks(parseToTree(source, options).children);
}

/** Set the parse cache byte cap (default 8 MiB). Evicts immediately when lowered. */
export function configureMarkdownParseCache(options: { maxBytes: number }): void {
  maxCacheBytes = Math.max(0, Math.floor(options.maxBytes));
  evict();
}
export function clearMarkdownParseCache(): void { cache.clear(); cacheBytes = 0; }
export function getMarkdownParseCacheEntryCount(): number { return cache.size; }
export function getMarkdownParseCacheBytes(): number { return cacheBytes; }
export type { Block } from "./detectors/index.js";
