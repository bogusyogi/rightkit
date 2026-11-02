import type { Root } from "mdast";
import {
  clearMarkdownParseCache as clearParseCache,
  configureMarkdownParseCache,
  getMarkdownParseCacheBytes as parseCacheBytes,
  getMarkdownParseCacheEntryCount as parseCacheEntryCount,
  parseToTree as parseTree,
  type ParseOptions,
} from "@rightkit/doc-render";
import { buildBlocks, type Block } from "./semanticBlocks.js";

export const TREE_CACHE_MAX_BYTES = 8 * 1024 * 1024;

// the host app renders `==text==` as <mark>; every other parser switch keeps the published default.
const PARSE_OPTIONS: ParseOptions = { highlight: true };

configureMarkdownParseCache({ maxBytes: TREE_CACHE_MAX_BYTES });

export function parseToTree(source: string): Root {
  return parseTree(source, PARSE_OPTIONS);
}

/** Parse markdown source into the gated block list the renderer consumes. */
export function parseMarkdown(source: string): Block[] {
  const tree = parseToTree(source);
  return buildBlocks(tree.children);
}

export function getMarkdownParseCacheEntryCount(): number {
  return parseCacheEntryCount();
}

export function getMarkdownParseCacheBytes(): number {
  return parseCacheBytes();
}

export function clearMarkdownParseCache(): void {
  clearParseCache();
}

export type { Block };
