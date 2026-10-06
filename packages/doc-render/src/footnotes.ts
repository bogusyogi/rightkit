import { createContext } from "react";
import type { Root, RootContent } from "mdast";

export interface Footnote {
  num: number;
  children: RootContent[];
}
export type FootnoteMap = Map<string, Footnote>;

/** Resolved footnote definitions for the current document, keyed by identifier. */
export const FootnotesContext = createContext<FootnoteMap | null>(null);

/** Collect footnote definitions (top-level nodes) and number them in document order. */
export function collectFootnotes(tree: Root): FootnoteMap {
  const map: FootnoteMap = new Map();
  let n = 0;
  for (const node of tree.children) {
    if ((node as { type: string }).type === "footnoteDefinition") {
      const def = node as unknown as { identifier: string; children: RootContent[] };
      if (!map.has(def.identifier)) {
        n += 1;
        map.set(def.identifier, { num: n, children: def.children ?? [] });
      }
    }
  }
  return map;
}

import { slug as ghSlug } from "github-slugger";

/** Heading slug matching GitHub's exact algorithm — so anchors match rendered GitHub READMEs. */
export function slug(text: string): string {
  return ghSlug(text);
}
