// Edit PDF mode session state (docs/PDF-EDITOR-NOW.md §3.2, §6, §12 "usePdfEditSession.ts").
//
// Owns mode/tool/selection/draft lifecycle, the ordered `sourceCommands` list, and digest/dirty
// checkpoint tracking. Does not own pdf.js's `AnnotationEditorUIManager` command history (§6.2,
// `pushSourceCommand` in pdfEditor.ts) or the real Tauri bridge (`tauriBridge.ts`, Dispatch D2) —
// this hook accepts bridge functions as injected parameters (§12's "injected-bridge-function
// pattern", typed in pdfEditModel.ts) so it is testable today against mocks. A caller that wires
// pdf.js's shared history can drive it through `apply`/`revert`/`publish`, which together satisfy
// pdfEditor.ts's `PdfSourceCommandSession<PdfEditCommand>` shape.
//
// No disk writes happen anywhere in this file (§3.2: "It never saves"). Save/checkpointing is
// triggered externally via `checkpoint()` once `commit_pdf_save` (§10.3) succeeds.

import { useCallback, useMemo, useRef, useState } from "react";
import {
  computeSourceDigest,
  isSessionDirty,
  type InspectPdfPage,
  type ListPdfFonts,
  type PdfAddText,
  type PdfEditCommand,
  type PdfEditSession,
  type PdfFontCatalogEntry,
  type PdfPageGraph,
  type PdfPoint,
  type PdfColor,
  type PdfRect,
  type PdfSelection,
  type PdfSourceEdit,
  type PdfTextAppearance,
  type PdfTextDraft,
} from "./pdfEditModel";

export type PdfEditSessionTool = PdfEditSession["tool"];

/** Bridge functions this hook needs, injected so it can be unit-tested without the real
 *  `tauriBridge.ts` (§12: real bridge lands at Dispatch D2). */
export interface PdfEditSessionBridge {
  inspectPdfPage: InspectPdfPage;
  listPdfFonts: ListPdfFonts;
}

export interface UsePdfEditSessionOptions {
  bridge: PdfEditSessionBridge;
  /** Authorized path passed to `inspectPdfPage` (§5.2). */
  sourcePath: string;
}

export interface PdfEditSessionController {
  session: PdfEditSession;
  /** Mutable latest session snapshot for async save callbacks; avoids stale render closures. */
  sessionRef: { current: PdfEditSession };
  dirty: boolean;

  // Mode / tool -----------------------------------------------------------------------------
  /** Enters Edit PDF mode (§3.2). Idempotent. */
  enterEditMode: () => void;
  /** §3.2 "Done": commits an active valid draft, then returns to View mode. Never saves. */
  exitEditMode: () => void;
  setTool: (tool: PdfEditSessionTool) => void;

  // Selection / draft lifecycle (§3.2, §6.1) -------------------------------------------------
  /** Selecting a new target commits any active valid draft first (§3.2 "Clicking another page
   *  target commits active valid draft, then selects target"). */
  selectIsland: (selection: PdfSelection) => void;
  clearSelection: () => void;
  /** Seeds a `sourceEdit` draft from the current selection's island text/appearance. */
  startSourceEditDraft: (params: {
    id: string;
    rect: PdfRect;
    text: string;
    appearance: PdfTextAppearance;
    availableBounds: PdfRect;
  }) => void;
  /** Seeds an `addText` draft at a clicked PDF-space anchor (§6, `PdfAddText.anchor`). */
  startAddTextDraft: (params: {
    id: string;
    page: number;
    anchor: PdfPoint;
    rect: PdfRect;
    appearance: PdfTextAppearance;
  }) => void;
  /** Seeds a persisted OCR correction overlay from recognized line geometry. */
  startOcrEditDraft: (params: {
    id: string;
    page: number;
    lineId: string;
    rect: PdfRect;
    text: string;
    appearance: PdfTextAppearance;
    coverColor: PdfColor;
  }) => void;
  updateDraftText: (text: string) => void;
  updateDraftAppearance: (appearance: PdfTextAppearance) => void;
  /** External layout measurement sets this (§3.3); this hook does not measure text itself. */
  setDraftOverflow: (overflow: boolean) => void;
  /** §3.2 "Clicking outside active text commits valid draft" / Done / selecting elsewhere.
   *  No-ops when there is no draft. A no-op (unchanged) `sourceEdit` draft commits nothing —
   *  matches §6's dirty semantics: an identical before/after would not need a history command.
   *  Returns the command that was applied, or null if nothing was committed. */
  commitDraft: () => PdfEditCommand | null;
  /** §3.2 Escape: "discards active draft without a history command." */
  cancelDraft: () => void;

  // Source command list / history plumbing (§6, §6.2) ----------------------------------------
  /** Applies `command` to `sourceCommands` and recomputes `sourceDigest`. Exposed directly (not
   *  only via `commitDraft`) so an external pdf.js command-history wrapper
   *  (`pushSourceCommand`, pdfEditor.ts) can drive apply/undo/redo through this same session. */
  apply: (command: PdfEditCommand) => void;
  /** Reverts the effect of `apply(command)` — removes the command by id. */
  revert: (command: PdfEditCommand) => void;
  /** Called after apply/revert reached through undo/redo (§9.4 in-memory preview recompose hook).
   *  This hook does not call `compose_pdf_preview` itself — it only tracks a subscriber list so a
   *  caller (PdfViewer) can react to every source-history mutation. */
  onPublish: (listener: () => void) => () => void;
  publish: () => void;

  // Checkpoint / annotation dirty (§6, §10.3) -------------------------------------------------
  /** Marks the current `sourceDigest` as saved and clears `annotationDirty` — call only after
   *  `commit_pdf_save` succeeds (§10.3: "checkpoints sourceDigest"). */
  checkpoint: () => void;
  setAnnotationDirty: (dirty: boolean) => void;

  // Source graph inspection (§5.2, §7.2) via injected bridge ---------------------------------
  inspectPage: (page: number) => Promise<PdfPageGraph>;
  listFonts: () => Promise<PdfFontCatalogEntry[]>;

  /** Resets session state to a fresh document (e.g. after `commit_pdf_save` reloads the active
   *  document, §10.3). Clears commands, selection, draft, and checkpoints to empty history. */
  resetForDocument: () => void;
}

const INITIAL_TOOL: PdfEditSessionTool = "selectText";

function initialSession(): PdfEditSession {
  const digest = computeSourceDigest([]);
  return {
    mode: "view",
    tool: INITIAL_TOOL,
    selection: null,
    draft: null,
    sourceCommands: [],
    sourceDigest: digest,
    savedSourceDigest: digest,
    annotationDirty: false,
  };
}

function draftIsNoOp(draft: PdfTextDraft): boolean {
  if (draft.kind === "addText") return draft.text.trim().length === 0;
  const sameText = draft.text === draft.originalText;
  const sameAppearance =
    !!draft.originalAppearance &&
    draft.appearance.fontId === draft.originalAppearance.fontId &&
    draft.appearance.sizePt === draft.originalAppearance.sizePt &&
    draft.appearance.color.r === draft.originalAppearance.color.r &&
    draft.appearance.color.g === draft.originalAppearance.color.g &&
    draft.appearance.color.b === draft.originalAppearance.color.b &&
    draft.appearance.color.alpha === draft.originalAppearance.color.alpha;
  return sameText && sameAppearance;
}

function draftToCommand(draft: PdfTextDraft): PdfEditCommand | null {
  if (draft.kind === "sourceEdit") {
    if (
      !draft.locator ||
      !draft.originalText ||
      !draft.originalAppearance ||
      !draft.availableBounds
    ) {
      return null;
    }
    const command: PdfSourceEdit = {
      kind: "replaceSourceText",
      id: draft.id,
      locator: draft.locator,
      before: { text: draft.originalText, appearance: draft.originalAppearance },
      after: { text: draft.text, appearance: draft.appearance },
      availableBounds: draft.availableBounds,
    };
    return command;
  }
  if (draft.kind === "ocrEdit") {
    if (!draft.lineId || !draft.coverColor) return null;
    return {
      kind: "correctOcrText",
      id: draft.id,
      page: draft.page,
      lineId: draft.lineId,
      rect: draft.rect,
      text: draft.text,
      appearance: draft.appearance,
      coverColor: draft.coverColor,
      backgroundProof: "nearUniform",
    };
  }
  if (!draft.anchor) return null;
  const command: PdfAddText = {
    kind: "addText",
    id: draft.id,
    page: draft.page,
    anchor: draft.anchor,
    text: draft.text,
    appearance: draft.appearance,
  };
  return command;
}

export function usePdfEditSession(options: UsePdfEditSessionOptions): PdfEditSessionController {
  const { bridge, sourcePath } = options;
  const [session, setSession] = useState<PdfEditSession>(initialSession);
  const sessionRef = useRef(session);
  sessionRef.current = session;
  const listenersRef = useRef<Set<() => void>>(new Set());
  const fontsCacheRef = useRef<Promise<PdfFontCatalogEntry[]> | null>(null);
  // Mirrors `session.draft`, refreshed every render, but ALSO nulled synchronously inside
  // `commitDraftInternal`/`cancelDraft` themselves (not only via their `setSession` updaters). A
  // caller (PdfViewer.tsx) can call `handleCommitDraft()` and then `selectIsland`/`exitEditMode`/an
  // annotation-mode switch in the same synchronous event handler — each of which also commits
  // internally — and React does not guarantee `session` (the render-closure value) reflects the
  // first commit's `draft: null` before the second one reads it. Reading/nulling this ref instead
  // means the second call sees the draft already cleared, instead of re-deriving and re-appending
  // the same command a second time before React ever re-renders.
  const draftRef = useRef<PdfTextDraft | null>(session.draft);
  draftRef.current = session.draft;
  // Mirrors `session.selection`, but is ALSO updated synchronously inside `selectIsland`/
  // `clearSelection` (not only via their `setSession` updaters). `startSourceEditDraft` reads it
  // instead of `session.selection`: PdfViewer calls `selectIsland(...)` and then
  // `startSourceEditDraft(...)` in the same synchronous click handler, and React has not
  // re-rendered between them, so the render-closure `session.selection` is still the pre-click
  // value (null on a first click) — which made `startSourceEditDraft` bail and no editor open on
  // the first click of an existing text run.
  const selectionRef = useRef<PdfSelection | null>(session.selection);
  selectionRef.current = session.selection;

  const dirty = useMemo(() => isSessionDirty(session), [session]);

  const publish = useCallback(() => {
    for (const listener of listenersRef.current) listener();
  }, []);

  const onPublish = useCallback((listener: () => void) => {
    listenersRef.current.add(listener);
    return () => {
      listenersRef.current.delete(listener);
    };
  }, []);

  const apply = useCallback((command: PdfEditCommand) => {
    setSession((prev) => {
      const sourceCommands = [...prev.sourceCommands, command];
      return { ...prev, sourceCommands, sourceDigest: computeSourceDigest(sourceCommands) };
    });
  }, []);

  const revert = useCallback((command: PdfEditCommand) => {
    setSession((prev) => {
      const index = prev.sourceCommands.findIndex((existing) => existing.id === command.id);
      if (index === -1) return prev;
      const sourceCommands = prev.sourceCommands.slice();
      sourceCommands.splice(index, 1);
      return { ...prev, sourceCommands, sourceDigest: computeSourceDigest(sourceCommands) };
    });
  }, []);

  const enterEditMode = useCallback(() => {
    setSession((prev) => (prev.mode === "edit" ? prev : { ...prev, mode: "edit" }));
  }, []);

  // Reads `draftRef.current` (not `session.draft` from the render-time closure, and not from
  // inside the `setSession` updater) so the command this returns is computed synchronously: React
  // does not guarantee a functional `setSession` updater runs before this function returns, so a
  // side effect set inside one cannot be relied on as this function's return value — and reading
  // `session.draft` directly would go stale across two synchronous calls in the same handler (see
  // `draftRef`'s doc comment above).
  const commitDraftInternal = useCallback((): PdfEditCommand | null => {
    const draft = draftRef.current;
    if (!draft) return null;
    draftRef.current = null;
    if (draftIsNoOp(draft)) {
      const current = sessionRef.current;
      if (current.draft) sessionRef.current = { ...current, draft: null };
      setSession((prev) => (prev.draft ? { ...prev, draft: null } : prev));
      return null;
    }
    const command = draftToCommand(draft);
    if (!command) {
      const current = sessionRef.current;
      if (current.draft) sessionRef.current = { ...current, draft: null };
      setSession((prev) => (prev.draft ? { ...prev, draft: null } : prev));
      return null;
    }
    const current = sessionRef.current;
    const sourceCommands = [...current.sourceCommands, command];
    sessionRef.current = {
      ...current,
      draft: null,
      sourceCommands,
      sourceDigest: computeSourceDigest(sourceCommands),
    };
    setSession((prev) => {
      const sourceCommands = [...prev.sourceCommands, command];
      return {
        ...prev,
        draft: null,
        sourceCommands,
        sourceDigest: computeSourceDigest(sourceCommands),
      };
    });
    return command;
  }, []);

  const commitDraft = useCallback((): PdfEditCommand | null => {
    const command = commitDraftInternal();
    if (command) publish();
    return command;
  }, [commitDraftInternal, publish]);

  const cancelDraft = useCallback(() => {
    draftRef.current = null;
    const current = sessionRef.current;
    if (current.draft) sessionRef.current = { ...current, draft: null };
    setSession((prev) => (prev.draft ? { ...prev, draft: null } : prev));
  }, []);

  const exitEditMode = useCallback(() => {
    commitDraftInternal();
    setSession((prev) => ({ ...prev, mode: "view", draft: null }));
  }, [commitDraftInternal]);

  const setTool = useCallback((tool: PdfEditSessionTool) => {
    setSession((prev) => (prev.tool === tool ? prev : { ...prev, tool }));
  }, []);

  const selectIsland = useCallback(
    (selection: PdfSelection) => {
      commitDraftInternal();
      selectionRef.current = selection;
      setSession((prev) => ({ ...prev, selection }));
    },
    [commitDraftInternal],
  );

  const clearSelection = useCallback(() => {
    selectionRef.current = null;
    setSession((prev) => (prev.selection ? { ...prev, selection: null } : prev));
  }, []);

  const startSourceEditDraft = useCallback(
    (params: {
      id: string;
      rect: PdfRect;
      text: string;
      appearance: PdfTextAppearance;
      availableBounds: PdfRect;
    }) => {
      const selection = selectionRef.current;
      if (!selection) return;
      const draft: PdfTextDraft = {
        kind: "sourceEdit",
        id: params.id,
        page: selection.page,
        rect: params.rect,
        text: params.text,
        appearance: params.appearance,
        locator: selection.locator,
        originalText: params.text,
        originalAppearance: params.appearance,
        availableBounds: params.availableBounds,
        anchor: null,
        overflow: false,
      };
      setSession((prev) => ({ ...prev, draft }));
    },
    // Reads `selectionRef.current` (see its doc comment), not `session.selection`, so this callback
    // needs no reactive dependency and stays stable across selection changes.
    [],
  );

  const startAddTextDraft = useCallback(
    (params: {
      id: string;
      page: number;
      anchor: PdfPoint;
      rect: PdfRect;
      appearance: PdfTextAppearance;
    }) => {
      const draft: PdfTextDraft = {
        kind: "addText",
        id: params.id,
        page: params.page,
        rect: params.rect,
        text: "",
        appearance: params.appearance,
        locator: null,
        originalText: null,
        originalAppearance: null,
        availableBounds: null,
        anchor: params.anchor,
        overflow: false,
      };
      setSession((prev) => ({ ...prev, draft }));
    },
    [],
  );

  const startOcrEditDraft = useCallback(
    (params: {
      id: string;
      page: number;
      lineId: string;
      rect: PdfRect;
      text: string;
      appearance: PdfTextAppearance;
      coverColor: PdfColor;
    }) => {
      const draft: PdfTextDraft = {
        kind: "ocrEdit",
        id: params.id,
        page: params.page,
        rect: params.rect,
        text: params.text,
        appearance: params.appearance,
        locator: null,
        originalText: params.text,
        originalAppearance: params.appearance,
        availableBounds: params.rect,
        anchor: null,
        lineId: params.lineId,
        coverColor: params.coverColor,
        backgroundProof: "nearUniform",
        overflow: false,
      };
      setSession((prev) => ({ ...prev, draft }));
    },
    [],
  );

  const updateDraftText = useCallback((text: string) => {
    setSession((prev) => (prev.draft ? { ...prev, draft: { ...prev.draft, text } } : prev));
  }, []);

  const updateDraftAppearance = useCallback((appearance: PdfTextAppearance) => {
    setSession((prev) => (prev.draft ? { ...prev, draft: { ...prev.draft, appearance } } : prev));
  }, []);

  const setDraftOverflow = useCallback((overflow: boolean) => {
    setSession((prev) =>
      prev.draft && prev.draft.overflow !== overflow
        ? { ...prev, draft: { ...prev.draft, overflow } }
        : prev,
    );
  }, []);

  const checkpoint = useCallback(() => {
    setSession((prev) => ({
      ...prev,
      savedSourceDigest: prev.sourceDigest,
      annotationDirty: false,
    }));
  }, []);

  const setAnnotationDirty = useCallback((next: boolean) => {
    setSession((prev) =>
      prev.annotationDirty === next ? prev : { ...prev, annotationDirty: next },
    );
  }, []);

  const inspectPage = useCallback(
    (page: number) => bridge.inspectPdfPage(sourcePath, page),
    [bridge, sourcePath],
  );

  const listFonts = useCallback(() => {
    if (!fontsCacheRef.current) {
      fontsCacheRef.current = bridge.listPdfFonts().catch((error) => {
        fontsCacheRef.current = null;
        throw error;
      });
    }
    return fontsCacheRef.current;
  }, [bridge]);

  const resetForDocument = useCallback(() => {
    fontsCacheRef.current = null;
    draftRef.current = null;
    selectionRef.current = null;
    setSession(initialSession());
  }, []);

  return {
    session,
    sessionRef,
    dirty,
    enterEditMode,
    exitEditMode,
    setTool,
    selectIsland,
    clearSelection,
    startSourceEditDraft,
    startAddTextDraft,
    startOcrEditDraft,
    updateDraftText,
    updateDraftAppearance,
    setDraftOverflow,
    commitDraft,
    cancelDraft,
    apply,
    revert,
    onPublish,
    publish,
    checkpoint,
    setAnnotationDirty,
    inspectPage,
    listFonts,
    resetForDocument,
  };
}
