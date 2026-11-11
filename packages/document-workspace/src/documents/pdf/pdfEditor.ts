// Stage 1 PDF editing — pure helpers shared between the toolbar and usePdfEditor. Kept dependency-
// free of pdfjs-dist's class exports (those are only imported where the real UIManager is built) so
// this module is trivial to unit test.

/** The editor tools this app exposes. "none" means the pure viewer (no AnnotationEditorLayer
 *  active) — the default, matching the view-first invariant. Values mirror pdf.js's own
 *  AnnotationEditorType numeric enum (kept here as a stable, decoupled mapping — see
 *  ANNOTATION_EDITOR_TYPE below — so this file doesn't need the real pdfjs-dist import to be
 *  unit-tested). */
export type PdfEditorMode = "none" | "freeText" | "ink" | "highlight" | "stamp";
export type PdfSaveFunction = () => Promise<Uint8Array | null>;

/** Mirrors pdfjs-dist's `AnnotationEditorType` numeric values (checked against
 *  node_modules/pdfjs-dist 6.2.108). Declared locally so pure helpers below don't need to import
 *  the real (heavy) pdfjs-dist module. usePdfEditor.ts cross-checks these against the live
 *  `pdfjs.AnnotationEditorType` export at runtime instead of trusting this duplication blindly. */
export const ANNOTATION_EDITOR_TYPE = {
  NONE: 0,
  FREETEXT: 3,
  HIGHLIGHT: 9,
  STAMP: 13,
  INK: 15,
} as const;

export function modeToAnnotationEditorType(mode: PdfEditorMode): number {
  switch (mode) {
    case "freeText":
      return ANNOTATION_EDITOR_TYPE.FREETEXT;
    case "ink":
      return ANNOTATION_EDITOR_TYPE.INK;
    case "highlight":
      return ANNOTATION_EDITOR_TYPE.HIGHLIGHT;
    case "stamp":
      return ANNOTATION_EDITOR_TYPE.STAMP;
    default:
      return ANNOTATION_EDITOR_TYPE.NONE;
  }
}

/**
 * pdf.js's low-level `TextLayer` renders into a caller-owned element, but
 * `AnnotationEditorLayer` expects the viewer-level shape `{ div }`. Keep that runtime contract
 * explicit so a raw `HTMLDivElement` or low-level `TextLayer` cannot be passed accidentally.
 */
export function createPdfEditorTextLayerAdapter(div: HTMLDivElement): {
  readonly div: HTMLDivElement;
} {
  return { div };
}

export interface PdfEditorToolDef {
  mode: PdfEditorMode;
  label: string;
  title: string;
}

// "Sign" here is a drawn/typed/image stamp placed on the page — NOT cryptographic PKI signing,
// which pdf.js does not implement. Keep the toolbar copy honest about that (see PdfToolbar.tsx).
export const PDF_EDITOR_TOOLS: PdfEditorToolDef[] = [
  { mode: "freeText", label: "text", title: "Add text" },
  { mode: "ink", label: "draw", title: "Draw (ink)" },
  { mode: "highlight", label: "highlight", title: "Highlight" },
  { mode: "stamp", label: "image", title: "Add image stamp" },
];

// ---------------------------------------------------------------------------------------------
// §6.2 unified document history — pin the pdf.js command-manager runtime surface
// ---------------------------------------------------------------------------------------------
//
// Installed pdf.js (checked against node_modules/pdfjs-dist 6.2.108) exposes
// `AnnotationEditorUIManager.addCommands` / `.undo` / `.redo` as its only public entry points into
// its internal `CommandManager` (node_modules/pdfjs-dist/build/pdf.mjs — `class CommandManager`,
// `class AnnotationEditorUIManager`). Every built-in annotation editor (FreeText size/color, Ink
// color/thickness, Highlight color/thickness, draw steps — grep `addCommands({` call sites in
// build/pdf.mjs) routes its own undo/redo through this exact call shape. Pushing source-text edits
// through the same call keeps annotation & source-text history in one chronological stack (§6.2)
// instead of two independent stacks that could interleave incorrectly.
//
// pdf.js's own shipped declaration types this loosely —
// `addCommands(params: Object): void` in node_modules/pdfjs-dist/types/src/display/editor/tools.d.ts
// — because AnnotationEditorUIManager is viewer-internal with no public stability contract (see
// usePdfEditor.ts's `AnyUIManager` comment for the same caveat at the constructor boundary).
// PdfCommandManagerCommand below is this app's own precise contract for that `params` object,
// reverse-engineered from `CommandManager#add`'s destructured parameter list. If a pdfjs-dist
// upgrade removes/renames `addCommands`/`undo`/`redo`, pdfEditor.test.ts's contract test fails
// before packaging — do not access CommandManager or #commandManager directly from app code.

/** This app's own precise contract for `AnnotationEditorUIManager.addCommands`'s single argument
 *  (pdf.js itself types it as a loose `Object` — see comment above). Field semantics reverse-
 *  engineered from `CommandManager#add` in node_modules/pdfjs-dist/build/pdf.mjs. */
export interface PdfCommandManagerCommand {
  /** Applies the command. Run synchronously inside `addCommands` when `mustExec` is true, and again
   *  on every `redo()`. */
  cmd: () => void;
  /** Reverts the command. Run on every `undo()`. */
  undo: () => void;
  /** Optional side effect run immediately after `undo`/`cmd` on `undo()`/`redo()` respectively (not
   *  on the initial `addCommands` call itself) — `CommandManager#undo`/`#redo` both call
   *  `post?.()` right after invoking `undo`/`cmd`. */
  post?: () => void;
  /** When true, `cmd` runs immediately inside `addCommands` (not deferred to first redo). Every
   *  call site inside pdf.js itself passes `true`. */
  mustExec: boolean;
  /** Numeric command-type tag consulted only by `overwriteIfSameType` (in-place replacement of the
   *  current history entry) and `cleanUndoStack(type)`; pdf.js never validates or restricts this
   *  value itself. See SOURCE_TEXT_COMMAND below for this app's reserved tag. */
  type?: number;
  /** When true, and the entry at the manager's current history position has a matching `type`,
   *  `addCommands` overwrites that entry in place instead of pushing a new history entry — used by
   *  pdf.js for live-dragging param updates (e.g. a font-size slider generating many intermediate
   *  values that should collapse to one undo step). */
  overwriteIfSameType?: boolean;
  /** Only meaningful together with `overwriteIfSameType: true` — when true, an in-place overwrite
   *  keeps the *original* entry's `undo` callback instead of replacing it with the new one. */
  keepUndo?: boolean;
}

/** The subset of pdf.js's `AnnotationEditorUIManager` this app relies on for §6.2's unified
 *  document history, typed precisely (see PdfCommandManagerCommand above) so call sites do not need
 *  usePdfEditor.ts's broader `AnyUIManager` escape hatch just to push one source-text command. */
export interface PdfCommandManagerHost {
  addCommands: (params: PdfCommandManagerCommand) => void;
  undo: () => void;
  redo: () => void;
}

/** This app's reserved `AnnotationEditorUIManager` command-type tag (§6.2) for source-text edits.
 *  Checked against every command-type value pdf.js itself uses internally
 *  (node_modules/pdfjs-dist/build/pdf.mjs): `AnnotationEditorParamsType` tops out at 41
 *  (DRAW_STEP) and `AnnotationEditorType` tops out at 102 (COMMENT) — neither collides with
 *  0x5652 (22098). Collision would not corrupt history regardless (this app always passes
 *  `overwriteIfSameType: false`, so `type` is never compared against pdf.js's own command types),
 *  but a distinct, verified-unique value keeps entries identifiable if that ever changes. */
export const SOURCE_TEXT_COMMAND = 0x5652;

/** Minimal source-edit session contract `pushSourceCommand` drives: applying, reverting, and
 *  publishing one staged edit. Kept generic over the command payload (`TCommand`) so this file does
 *  not couple to whichever module currently owns the concrete PdfEditCommand/session-controller
 *  shape (§6, §12 — usePdfEditSession.ts) — matching this file's existing house style of staying
 *  dependency-free of heavier pdf.js/session types (see file header). */
export interface PdfSourceCommandSession<TCommand> {
  /** Applies `command` to the session's staged source-edit state (e.g. append/replace it in
   *  `sourceCommands`, recompute `sourceDigest`). */
  apply: (command: TCommand) => void;
  /** Reverts the effect of `apply(command)`. */
  revert: (command: TCommand) => void;
  /** Runs after every apply/revert reached through undo or redo — the hook for the §9.4 in-memory
   *  preview recompose. Not invoked by `pushSourceCommand` itself on the initial push (mirrors
   *  `CommandManager#add`, which only calls `post` from `undo()`/`redo()`, never from `add()`). */
  publish: () => void;
}

/**
 * Pushes one source-text edit onto pdf.js's shared `AnnotationEditorUIManager` command history
 * (§6.2) so source-text and annotation edits undo/redo in one chronological order instead of two
 * independent stacks. Do not maintain a second, independent undo stack for source edits.
 */
export function pushSourceCommand<TCommand>(
  uiManager: PdfCommandManagerHost,
  session: PdfSourceCommandSession<TCommand>,
  command: TCommand,
): void {
  uiManager.addCommands({
    cmd: () => session.apply(command),
    undo: () => session.revert(command),
    post: () => session.publish(),
    mustExec: true,
    type: SOURCE_TEXT_COMMAND,
    overwriteIfSameType: false,
    keepUndo: false,
  });
}
