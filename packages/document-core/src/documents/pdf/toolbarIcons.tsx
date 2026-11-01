// PDF toolbar icons — Phosphor, matching the existing zoom/rotate icons in PdfToolbar.tsx
// (14px, regular weight, aria-hidden — the accessible name lives on the wrapping button's
// aria-label/title) and the DOCX toolbar's icon-based convention (src/documents/docx/toolbarIcons.tsx).
import { SquaresFour } from "@phosphor-icons/react/dist/icons/SquaresFour";
import { TreeStructure } from "@phosphor-icons/react/dist/icons/TreeStructure";
import { ArrowsOutLineHorizontal } from "@phosphor-icons/react/dist/icons/ArrowsOutLineHorizontal";
import { CornersOut } from "@phosphor-icons/react/dist/icons/CornersOut";
import { TextT } from "@phosphor-icons/react/dist/icons/TextT";
import { TextAa } from "@phosphor-icons/react/dist/icons/TextAa";
import { Scribble } from "@phosphor-icons/react/dist/icons/Scribble";
import { Highlighter } from "@phosphor-icons/react/dist/icons/Highlighter";
import { ImageSquare } from "@phosphor-icons/react/dist/icons/ImageSquare";

/** Page thumbnails panel toggle. */
export const IconThumbnails = () => <SquaresFour size={14} weight="regular" aria-hidden />;
/** Outline (bookmarks) panel toggle. */
export const IconOutline = () => <TreeStructure size={14} weight="regular" aria-hidden />;
/** Fit to width. */
export const IconFitWidth = () => (
  <ArrowsOutLineHorizontal size={14} weight="regular" aria-hidden />
);
/** Fit full page. */
export const IconFitPage = () => <CornersOut size={14} weight="regular" aria-hidden />;
/** Toggle existing-text edit mode (edits source text runs, not overlaid annotations). */
export const IconEditText = () => <TextT size={14} weight="regular" aria-hidden />;
/** Add-text annotation tool. */
export const IconAnnotateText = () => <TextAa size={14} weight="regular" aria-hidden />;
/** Ink/draw annotation tool. */
export const IconDraw = () => <Scribble size={14} weight="regular" aria-hidden />;
/** Highlight annotation tool. */
export const IconHighlight = () => <Highlighter size={14} weight="regular" aria-hidden />;
/** Image stamp annotation tool. */
export const IconImage = () => <ImageSquare size={14} weight="regular" aria-hidden />;
