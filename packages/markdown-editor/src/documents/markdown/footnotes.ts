import { createContext } from "react";
import { slug as ghSlug } from "github-slugger";
import type { collectFootnotes } from "@rightkit/doc-render";

export { collectFootnotes } from "@rightkit/doc-render";
export type FootnoteMap = ReturnType<typeof collectFootnotes>;

/** Resolved footnote definitions for the current document, keyed by identifier. */
export const FootnotesContext = createContext<FootnoteMap | null>(null);

/** Heading slug matching GitHub's exact algorithm — so anchors match rendered GitHub READMEs. */
export function slug(text: string): string {
  return ghSlug(text);
}
