import { createContext } from "react";
import GithubSlugger from "github-slugger";
import type { Root, RootContent, Heading } from "mdast";
import { nodeText } from "./detectors/nodeText.js";

// Per-document heading → slug map. Built in a single document-order pass with one GithubSlugger
// instance so duplicate headings get -1/-2 suffixes (matching GitHub), and the TOC and the rendered
// headings read the SAME ids. Computed once in a useMemo (pure) rather than mutating a slugger
// during render, which would break under StrictMode/concurrent rendering.
export type HeadingSlugMap = Map<Heading, string>;

export const HeadingSlugContext = createContext<HeadingSlugMap | null>(null);

export function buildHeadingSlugs(tree: Root): HeadingSlugMap {
  const slugger = new GithubSlugger();
  const map: HeadingSlugMap = new Map();
  const walk = (nodes: RootContent[]) => {
    for (const n of nodes) {
      if (n.type === "heading") map.set(n as Heading, slugger.slug(nodeText(n)));
      const kids = (n as { children?: RootContent[] }).children;
      if (kids) walk(kids);
    }
  };
  walk(tree.children);
  return map;
}
