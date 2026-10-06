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

const processor = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkFrontmatter, ["yaml"])
  .use(remarkMath)
  .use(remarkWikiLink)
  .use(remarkDefListAbbr)
  .use(remarkCitations)
  .use(remarkSmartypants);

const cache = new Map<string, Root>();
const MAX_CACHE = 8;

export function parseToTree(source: string): Root {
  const cached = cache.get(source);
  if (cached) {
    cache.delete(source);
    cache.set(source, cached);
    return cached;
  }
  const tree = processor.runSync(processor.parse(source)) as Root;
  cache.set(source, tree);
  while (cache.size > MAX_CACHE) cache.delete(cache.keys().next().value as string);
  return tree;
}

export function parseMarkdown(source: string): Block[] {
  return buildBlocks(parseToTree(source).children);
}

export function clearMarkdownParseCache(): void { cache.clear(); }
export function getMarkdownParseCacheEntryCount(): number { return cache.size; }
export type { Block } from "./detectors/index.js";
