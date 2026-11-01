import type { DocFormat } from "@rightkit/document-core/documents/types";

// Single source of truth for what the shell may do with a given format. Replaces ad-hoc
// `format === "docx"` / EDITABLE-set checks scattered across App and the action bar, so adding a
// format means updating this map — not hunting branches.
export interface DocumentCapabilities {
  /** Edited inside the host app, either through a source surface, the DOCX bridge, or the image shell. */
  canEdit: boolean;
  /** Offers Save As for editable local formats. */
  canSaveAs: boolean;
  /** Printable via the shell's print path. DOCX prints through its own editor bridge instead. */
  canPrintInApp: boolean;
  /** Has a shell-level preview/source view toggle. */
  canSwitchView: boolean;
  /** Find supports replace (text formats). */
  canReplace: boolean;
}

const EDITABLE = new Set<DocFormat>(["txt", "md", "mdx", "html", "docx", "pdf", "image"]);
// md/mdx (preview/source), html (interactive/safe + source drawer). Images edit inside the viewer shell.
const VIEW_SWITCH = new Set<DocFormat>(["md", "mdx", "html"]);
const TEXT_REPLACE = new Set<DocFormat>(["txt", "md", "mdx", "html"]);

/** Single entry point for format capabilities. Callers read `capabilitiesFor(format).canReplace`. */
export function capabilitiesFor(format: DocFormat): DocumentCapabilities {
  return {
    canEdit: EDITABLE.has(format),
    canSaveAs: EDITABLE.has(format),
    canPrintInApp: format !== "docx" && format !== "web" && format !== "unknown",
    canSwitchView: VIEW_SWITCH.has(format),
    canReplace: TEXT_REPLACE.has(format),
  };
}

/** md and mdx share one Markdown pipeline; use this instead of `format === "md" || format === "mdx"`. */
export function isMarkdownFormat(format: DocFormat | undefined): boolean {
  return format === "md" || format === "mdx";
}
