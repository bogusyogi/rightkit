// Page-slot inline text editor (docs/PDF-EDITOR-NOW.md §6.1, §12
// "src/documents/pdf/PdfInlineTextEditor.tsx | Page-slot contenteditable/input, click-away,
// Escape, composition, overflow").
//
// Renders inside the same absolute page slot as the canvas/text/annotation layers, positioned at
// a caller-supplied screen-space rect, editing one `PdfTextDraft` (either an existing-island
// `sourceEdit` draft or a fresh `addText` draft; see `pdfEditModel.ts`, frozen).
//
// Prop-contract judgment calls (pdfEditModel.ts is frozen and defines neither a screen-rect type
// nor this component's props, so these are resolved here, not invented there):
//
// 1. Screen-space rect: `PdfTextIsland`/`PdfTextDraft` carry only PDF-space geometry (`PdfRect`,
//    §3.2's `pdfRectToViewport(session.draft.rect, viewport)`). The caller (PdfViewer) already
//    owns the viewport and must do that conversion; this component receives the already-converted
//    rect as plain CSS pixels. Reusing the frozen `PdfRect` shape for it (rather than inventing a
//    `PdfViewportRect`) keeps the field names (`x`, `y`, `width`, `height`) consistent with the
//    rest of the contract — only the unit (CSS px vs PDF user-space units) differs, and that unit
//    is documented on the prop itself.
// 2. `onCommit`/`onCancel` are zero-argument, matching the exact §6.1 usage snippet
//    (`onCommit={commitDraft}`, `onCancel={cancelDraft}`, both zero-arg methods on
//    `PdfEditSessionController`). Live text edits are not part of that snippet's shape, so they are
//    threaded back through a separate `onTextChange(text)` prop (wired by the caller to
//    `session.updateDraftText`) rather than overloading `onCommit` with an argument that would
//    break the documented call site.
// 3. Overflow: `pdfEditModel.ts`'s `PdfTextDraft.overflow` is the authoritative flag (read by Save
//    to disable that command, §3.3), and `usePdfEditSession.ts` exposes a matching zero-arg-return
//    setter `setDraftOverflow(overflow: boolean)`. This component does not call that setter
//    directly (it has no session reference) — it reports the same boolean shape via an
//    `onOverflowChange(overflow: boolean)` prop so the caller can wire
//    `onOverflowChange={session.setDraftOverflow}` verbatim. Overflow is measured against the
//    caller-supplied screen rect (the on-screen box `availableBounds` maps to), comparing the
//    editable element's scrollWidth/scrollHeight to its clientWidth/clientHeight — the same
//    "does the content option outgrow its box" signal `validate_layout` (§8) checks in PDF space.
//
// Appearance -> CSS conversion: `PdfTextAppearance.sizePt` is in PDF points (1/72in); CSS px
// assumes 96dpi, so `px = pt * 96/72`. `color` fields are 0-1 floats (`{r,g,b,alpha}`); CSS wants
// 0-255 integers for r/g/b and 0-1 for alpha, so only r/g/b are scaled.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type CompositionEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import type { PdfColor, PdfRect, PdfTextAppearance, PdfTextDraft } from "./pdfEditModel";

/** Default CSS px per PDF user-space unit when the caller does not supply the live viewport scale:
 *  96dpi CSS px over 72dpi PDF points, i.e. a viewer at 100% with pdf.js's PDF_TO_CSS_UNITS. */
export const DEFAULT_CSS_PER_PDF_UNIT = 96 / 72;

export interface PdfInlineTextEditorProps {
  /** The in-progress draft being edited (existing-island `sourceEdit` or fresh `addText`). */
  draft: PdfTextDraft;
  /** Live CSS px per PDF user-space unit for the draft's page — `PageViewport.scale` from the same
   *  viewport `rect` was converted with. Without it the editor's glyphs render at a fixed 100% size
   *  while the page underneath is zoomed, which is exactly the size mismatch users saw. */
  cssPerPdfUnit?: number;
  /** Background painted behind the editable so the ORIGINAL glyphs (still on the canvas underneath
   *  for a `sourceEdit` draft) don't show through the text being typed over them. The caller
   *  samples this from the page canvas (see `sampleCanvasRectBackground`); `null`/undefined leaves
   *  the editor transparent (right for `addText` over empty page space). */
  coverColor?: PdfColor | null;
  /** Screen-space (CSS pixel) rect for this draft, already converted from PDF-space via
   *  `pdfRectToViewport` by the caller — see judgment call 1 above. Reuses the frozen `PdfRect`
   *  field shape; unit is CSS px here, not PDF user-space units. */
  rect: PdfRect;
  /** Fired on every keystroke with the editable's current plain-text content, so the caller can
   *  keep `session.draft.text` in sync (`usePdfEditSession.ts`'s `updateDraftText`). */
  onTextChange: (text: string) => void;
  /** §3.2: Enter, click-away (blur), or an external "commit the active draft" call (Done, page
   *  switch, another selection) should all resolve here. Zero-arg to match §6.1's
   *  `onCommit={commitDraft}` usage exactly. */
  onCommit: () => void;
  /** §3.2 Escape: "discards active draft without a history command." Zero-arg to match §6.1's
   *  `onCancel={cancelDraft}` usage exactly. */
  onCancel: () => void;
  /** §3.3: fires whenever the measured overflow state changes, so the caller can mirror it onto
   *  `session.draft.overflow` via `setDraftOverflow`. Optional because a caller doing its own
   *  layout measurement elsewhere is not required to wire this. */
  onOverflowChange?: (overflow: boolean) => void;
}

/**
 * Maps a PDF font resource name to CSS font properties. PDF names are not CSS families: embedded
 * subsets arrive as `ABCDEF+SourceSerif4-Bold`, base-14 fonts as `Helvetica-BoldOblique`, and
 * TrueType names as `Arial,BoldItalic`. Handing those to `font-family` verbatim falls back to the
 * browser default (a serif), which is the wrong-face rendering users saw over sans-serif source
 * text. The style suffix is peeled into weight/style, and the family gets a generic fallback
 * chosen from the base name so an unknown embedded face at least lands in the right class.
 */
export function pdfFontToCss(
  family: string,
): Pick<CSSProperties, "fontFamily" | "fontWeight" | "fontStyle"> {
  const trimmed = family.trim().replace(/^[A-Z]{6}\+/, "");
  const [rawBase, ...suffixParts] = trimmed.split(/[-,]/);
  const styleText = `${suffixParts.join(" ")} ${rawBase}`.toLowerCase();
  const bold = /bold|black|heavy|semibold|demibold/.test(styleText);
  const italic = /italic|oblique/.test(styleText);
  // Split CamelCase / digits so "SourceSerif4" becomes a name a CSS lookup can match ("Source
  // Serif 4") when that font is installed or bundled.
  const base = rawBase
    .replace(/([a-z])([A-Z0-9])/g, "$1 $2")
    .replace(/([A-Za-z])(\d)/g, "$1 $2")
    .replace(/\s+/g, " ")
    .trim();
  const lower = base.toLowerCase();
  let generic = "sans-serif";
  if (/courier|mono|consol|menlo|monaco/.test(lower)) generic = "monospace";
  else if (
    /times|serif|georgia|garamond|book|roman|cambria|palatino|minion|baskerville|caslon/.test(
      lower,
    ) &&
    !/sans/.test(lower)
  ) {
    generic = "serif";
  }
  const fontFamily = base ? `"${base}", ${generic}` : generic;
  return {
    fontFamily,
    fontWeight: bold ? 700 : 400,
    fontStyle: italic ? "italic" : "normal",
  };
}

function appearanceToStyle(
  appearance: PdfTextAppearance,
  rect: PdfRect,
  cssPerPdfUnit: number,
  coverColor: PdfColor | null | undefined,
): CSSProperties {
  const { r, g, b, alpha } = appearance.color;
  const cover = coverColor
    ? `rgba(${Math.round(coverColor.r * 255)}, ${Math.round(coverColor.g * 255)}, ${Math.round(coverColor.b * 255)}, ${coverColor.alpha})`
    : "transparent";
  return {
    position: "absolute",
    left: rect.x,
    top: rect.y,
    width: rect.width,
    height: rect.height,
    ...pdfFontToCss(appearance.family),
    fontSize: `${appearance.sizePt * cssPerPdfUnit}px`,
    color: `rgba(${Math.round(r * 255)}, ${Math.round(g * 255)}, ${Math.round(b * 255)}, ${alpha})`,
    // The rect is the island's glyph box (ascender to descender), so centering one line in it
    // lands the glyphs where the source run sits far better than lineHeight:1 at the box top did.
    lineHeight: `${rect.height}px`,
    whiteSpace: "pre",
    overflow: "visible",
    outline: "none",
    background: cover,
    border: "none",
    padding: 0,
    margin: 0,
    cursor: "text",
  };
}

/**
 * Page-slot inline editor for one text draft. Uses a `contenteditable` div (§12's
 * "contenteditable/input") rather than an `<input>`/`<textarea>` because the draft's appearance
 * (arbitrary embedded font, PDF point size) must render exactly as it will in the composed
 * candidate, and a plain form control cannot host that plus multi-run styling if it is extended
 * later; a single style span is sufficient for the current single-appearance-per-draft contract.
 */
export function PdfInlineTextEditor(props: PdfInlineTextEditorProps): ReactNode {
  const { draft, rect, onTextChange, onCommit, onCancel, onOverflowChange } = props;

  const editableRef = useRef<HTMLDivElement | null>(null);
  const composingRef = useRef(false);
  const overflowRef = useRef(false);
  const [overflowed, setOverflowed] = useState(draft.overflow);
  // Reading the latest callbacks from refs (rather than depending on them in effects/handlers)
  // avoids re-binding DOM listeners or re-running layout measurement solely because the caller
  // passed a new function identity, and, for onCommit/onCancel specifically, guarantees the
  // handler that actually runs at blur/keydown time is not a stale closure over an earlier draft.
  const onTextChangeRef = useRef(onTextChange);
  onTextChangeRef.current = onTextChange;
  const onCommitRef = useRef(onCommit);
  onCommitRef.current = onCommit;
  const onCancelRef = useRef(onCancel);
  onCancelRef.current = onCancel;
  const onOverflowChangeRef = useRef(onOverflowChange);
  onOverflowChangeRef.current = onOverflowChange;

  // Seed / re-seed DOM content when the draft identity changes (new draft started), not on every
  // keystroke — otherwise typing would fight the controlled `draft.text` echoed back a render
  // later and the caret would jump to the end on every character.
  const draftIdRef = useRef<string | null>(null);
  useEffect(() => {
    const el = editableRef.current;
    if (!el) return;
    if (draftIdRef.current === draft.id) return;
    draftIdRef.current = draft.id;
    overflowRef.current = draft.overflow;
    setOverflowed(draft.overflow);
    el.textContent = draft.text;
    el.focus();
    // Place caret at end of content.
    const selection = window.getSelection();
    if (selection) {
      const range = document.createRange();
      range.selectNodeContents(el);
      range.collapse(false);
      selection.removeAllRanges();
      selection.addRange(range);
    }
  }, [draft.id, draft.text, draft.overflow]);

  const draftRef = useRef(draft);
  draftRef.current = draft;

  const measureOverflow = useCallback(() => {
    const el = editableRef.current;
    if (!el) return;
    const current = draftRef.current;
    // Unchanged text can't overflow the run it came from: the CSS fallback face is never a
    // metric-exact match for the embedded PDF font, so measuring the untouched original against
    // the source glyph box produced a false "Text exceeds available space" the moment the editor
    // opened. Only text the user actually changed is measured, with a 1px rounding allowance.
    const unchanged = current.originalText !== null && current.text === current.originalText;
    const overflow =
      !unchanged && (el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1);
    if (overflow !== overflowRef.current) {
      overflowRef.current = overflow;
      setOverflowed(overflow);
      onOverflowChangeRef.current?.(overflow);
    }
  }, []);

  // Re-measure whenever text, appearance, or the available rect changes — all three affect
  // whether content fits (§3.3: overflow is checked against `availableBounds`, and appearance
  // changes come from the toolbar while this element stays mounted). measureOverflow itself reads
  // editableRef's live DOM measurements, not these values directly, but they must stay listed so
  // the effect actually re-runs when any of them changes.
  // biome-ignore lint/correctness/useExhaustiveDependencies: see comment above.
  useLayoutEffect(() => {
    measureOverflow();
  }, [draft.text, draft.appearance, rect.width, rect.height, measureOverflow]);

  const handleInput = useCallback(() => {
    const el = editableRef.current;
    if (!el) return;
    onTextChangeRef.current(el.textContent ?? "");
  }, []);

  const handleCompositionStart = useCallback((_event: CompositionEvent<HTMLDivElement>) => {
    composingRef.current = true;
  }, []);

  const handleCompositionEnd = useCallback((event: CompositionEvent<HTMLDivElement>) => {
    composingRef.current = false;
    onTextChangeRef.current(event.currentTarget.textContent ?? "");
  }, []);

  const handleKeyDown = useCallback((event: KeyboardEvent<HTMLDivElement>) => {
    // While an IME composition is active, Enter/Escape belong to the IME (candidate selection),
    // not to this editor's commit/cancel — committing mid-composition would persist an
    // intermediate, not-yet-confirmed string. `event.nativeEvent.isComposing` covers the case
    // some browsers still report `keyCode === 229` for; jsdom user-event does not set that, so
    // the `composingRef` flag driven by our own composition handlers is the reliable signal here.
    if (composingRef.current || event.nativeEvent.isComposing) return;

    if (event.key === "Enter") {
      event.preventDefault();
      onCommitRef.current();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      onCancelRef.current();
    }
  }, []);

  // §3.2: "Clicking outside active text commits valid draft." Blur is the DOM signal for
  // click-away (or focus moving to the toolbar, which per §3.2 stays "inside edit interaction
  // scope" — that exemption is the caller's responsibility via focus management / event ordering
  // in PdfViewer.tsx, not something this leaf component can see).
  const handleBlur = useCallback(() => {
    if (composingRef.current) return;
    onCommitRef.current();
  }, []);

  return (
    <>
      <div
        ref={editableRef}
        role="textbox"
        aria-label={`Edit PDF text on page ${draft.page}`}
        aria-multiline="false"
        aria-invalid={overflowed || undefined}
        aria-describedby={overflowed ? `pdf-inline-text-editor-feedback-${draft.id}` : undefined}
        tabIndex={0}
        contentEditable
        suppressContentEditableWarning
        data-pdf-inline-text-editor=""
        data-draft-kind={draft.kind}
        data-draft-id={draft.id}
        style={appearanceToStyle(
          draft.appearance,
          rect,
          props.cssPerPdfUnit ?? DEFAULT_CSS_PER_PDF_UNIT,
          props.coverColor,
        )}
        onInput={handleInput}
        onCompositionStart={handleCompositionStart}
        onCompositionEnd={handleCompositionEnd}
        onKeyDown={handleKeyDown}
        onBlur={handleBlur}
      />
      {overflowed && (
        <span id={`pdf-inline-text-editor-feedback-${draft.id}`} className="sr-only">
          Text exceeds available space.
        </span>
      )}
    </>
  );
}
