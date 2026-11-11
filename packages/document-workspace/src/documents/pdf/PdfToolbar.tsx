import { Minus } from "@phosphor-icons/react/dist/icons/Minus";
import { Plus } from "@phosphor-icons/react/dist/icons/Plus";
import { ArrowCounterClockwise } from "@phosphor-icons/react/dist/icons/ArrowCounterClockwise";
import { ArrowClockwise } from "@phosphor-icons/react/dist/icons/ArrowClockwise";
import { Scan } from "@phosphor-icons/react/dist/icons/Scan";
import { useRef, useState, type ReactNode } from "react";
import { DocumentToolbarSearch, type DocumentSearchMode } from "@rightkit/document-core/documents/DocumentToolbarFind";
import { DocumentToolbarShell } from "@rightkit/document-core/documents/DocumentToolbarShell";
import { PDF_EDITOR_TOOLS, type PdfEditorMode } from "./pdfEditor.js";
import type { PdfOcrDocumentState, PdfOcrStatus } from "./usePdfOcr.js";
import {
  IconThumbnails,
  IconOutline,
  IconFitWidth,
  IconFitPage,
  IconEditText,
  IconAnnotateText,
  IconDraw,
  IconHighlight,
  IconImage,
} from "./toolbarIcons.js";
import { DocBar } from "@rightkit/document-core/app/shell/docBar";

// Maps each Stage 1 annotation-editor tool (defined in pdfEditor.ts, kept dependency-free of
// React) to its toolbar icon. "none" never appears in PDF_EDITOR_TOOLS so it has no icon.
const EDITOR_TOOL_ICON: Partial<Record<PdfEditorMode, ReactNode>> = {
  freeText: <IconAnnotateText />,
  ink: <IconDraw />,
  highlight: <IconHighlight />,
  stamp: <IconImage />,
};

// Presentational PDF toolbar: panel toggles (thumbnails/outline), page box, zoom, fit, rotate, and
// (Stage 1) the annotation-editor tool switcher. All state lives in PdfViewer; this is
// props-in / callbacks-out.
export function PdfToolbar({
  panel,
  hasOutline,
  current,
  numPages,
  pageDraft,
  pageShake,
  scaleDraft,
  fitMode,
  onTogglePanel,
  onPageDraftChange,
  onCommitPageDraft,
  onZoom,
  onScaleDraftChange,
  onCommitScaleDraft,
  onSetFit,
  onRotate,
  ocrAvailable,
  currentPageScanned,
  ocrState = "idle",
  ocrErrorMessage,
  onRecognize,
  documentOcrState,
  onRecognizeDocument,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  editorMode,
  editorReady,
  editorDirty,
  onSetEditorMode,
  freeTextSize,
  onFreeTextSizeChange,
  onAddImageStamp,
  textEditActive,
  onToggleTextEdit,
  readOnly = false,
}: {
  panel: null | "outline" | "thumbs";
  hasOutline: boolean;
  current: number;
  numPages: number;
  pageDraft: string | null;
  pageShake: boolean;
  scaleDraft: string;
  fitMode: "width" | "page" | null;
  onTogglePanel: (p: "outline" | "thumbs") => void;
  onPageDraftChange: (v: string) => void;
  onCommitPageDraft: () => void;
  onZoom: (delta: number) => void;
  onScaleDraftChange: (v: string) => void;
  onCommitScaleDraft: () => void;
  onSetFit: (mode: "width" | "page") => void;
  onRotate: (delta: number) => void;
  ocrAvailable?: boolean;
  currentPageScanned?: boolean;
  ocrState?: PdfOcrStatus;
  ocrErrorMessage?: string;
  onRecognize?: () => void;
  documentOcrState?: PdfOcrDocumentState;
  onRecognizeDocument?: () => void;
  findBar?: ReactNode;
  findMode?: DocumentSearchMode;
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  /** Stage 1 editor tool state — "none" is the pure-viewer default (view-first invariant). */
  editorMode?: PdfEditorMode;
  editorReady?: boolean;
  editorDirty?: boolean;
  onSetEditorMode?: (mode: PdfEditorMode) => void;
  freeTextSize?: number;
  onFreeTextSizeChange?: (size: number) => void;
  onAddImageStamp?: (file: File) => undefined | Promise<boolean>;
  /** Existing-text edit mode — separate from the annotation-editor tools above (it edits source
   *  text runs via the backend, not overlaid annotations). Off by default (view-first). */
  textEditActive?: boolean;
  onToggleTextEdit?: () => void;
  /** Wave 2 Read mode — every markup/edit affordance hides; only the viewing pills remain. */
  readOnly?: boolean;
}) {
  const stampInputRef = useRef<HTMLInputElement>(null);
  // Floating-bar icon button (34px round) — matches TBtn round chrome without a wrapper component.
  const capsuleBtn = (active: boolean) =>
    `vr-press vr-focus grid h-[34px] w-[34px] flex-shrink-0 place-items-center rounded-full border transition-colors disabled:opacity-30 ${
      active
        ? "border-copper bg-copper/15 text-copper-hi"
        : "border-transparent text-muted hover:border-line hover:text-paper"
    }`;
  // Single capsule docked in its own row above the pages (data-vr-floating-bar: reader-surface
  // strip + hairline), so scrolled pages end at the row instead of sliding under it; the find
  // capsule hangs 8px under the row.
  return (
    <div className="no-print vr-chrome vr-document-toolbar relative z-30 shrink-0 text-xs text-muted">
      <DocBar>
        <div className="vr-capsule h-[42px] px-1">
          <DocumentToolbarShell
            ariaLabel="PDF tools"
            variant="scroll"
            className="vr-capsule-scroll"
          >
            {/* View group — panels + page nav stay in Read (view-first). */}
            <button
              type="button"
              onClick={() => onTogglePanel("thumbs")}
              aria-pressed={panel === "thumbs"}
              aria-label="Page thumbnails"
              className={capsuleBtn(panel === "thumbs")}
              title="Page thumbnails"
            >
              <IconThumbnails />
            </button>
            <button
              type="button"
              onClick={() => onTogglePanel("outline")}
              disabled={!hasOutline}
              aria-pressed={panel === "outline"}
              aria-label={hasOutline ? "Outline bookmarks" : "No outline in this PDF"}
              className={capsuleBtn(panel === "outline")}
              title={hasOutline ? "Outline (bookmarks)" : "No outline in this PDF"}
            >
              <IconOutline />
            </button>
            <span className="mx-1 h-5 w-px flex-shrink-0 bg-line" />
            <input
              type="number"
              value={pageDraft ?? current}
              aria-label="Page number"
              onChange={(e) => onPageDraftChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  onCommitPageDraft();
                  (e.target as HTMLInputElement).blur();
                }
              }}
              onBlur={onCommitPageDraft}
              className={`w-12 flex-shrink-0 rounded-md border bg-ink px-1 py-1 text-center font-mono text-paper outline-none focus:border-copper ${pageShake ? "vr-shake border-danger" : "border-line"}`}
            />
            <span className="flex-shrink-0 whitespace-nowrap font-mono text-muted-2">
              / {numPages || "…"}
            </span>
            {/* Markup group — Edit only; every markup/edit affordance hides in Read. */}
            {!readOnly && (
              <>
                {(onToggleTextEdit || onSetEditorMode) && (
                  <span className="mx-1 h-5 w-px flex-shrink-0 bg-line" />
                )}
                {onToggleTextEdit && (
                  <button
                    type="button"
                    onClick={onToggleTextEdit}
                    aria-pressed={!!textEditActive}
                    aria-label="Edit PDF text"
                    title="Edit existing PDF text"
                    className={capsuleBtn(!!textEditActive)}
                  >
                    <IconEditText />
                  </button>
                )}
                {onSetEditorMode && (
                  <>
                    {PDF_EDITOR_TOOLS.map((tool) => (
                      <button
                        key={tool.mode}
                        type="button"
                        disabled={!editorReady}
                        onClick={() => {
                          if (tool.mode === "stamp") {
                            onSetEditorMode("stamp");
                            stampInputRef.current?.click();
                            return;
                          }
                          onSetEditorMode(editorMode === tool.mode ? "none" : tool.mode);
                        }}
                        aria-pressed={editorMode === tool.mode}
                        aria-label={tool.title}
                        title={tool.title}
                        className={capsuleBtn(editorMode === tool.mode)}
                      >
                        {EDITOR_TOOL_ICON[tool.mode]}
                      </button>
                    ))}
                    {editorMode === "freeText" && onFreeTextSizeChange && (
                      <AddedTextSizeInput
                        value={freeTextSize ?? 10}
                        onChange={onFreeTextSizeChange}
                      />
                    )}
                  </>
                )}
                {editorDirty && (
                  <span
                    role="img"
                    className="ml-1 flex-shrink-0 text-copper-hi"
                    title="Unsaved annotation edits"
                    aria-label="Unsaved annotation edits"
                  >
                    ●
                  </span>
                )}
                <input
                  ref={stampInputRef}
                  type="file"
                  accept="image/*"
                  className="hidden"
                  onChange={(e) => {
                    const file = e.target.files?.[0];
                    e.target.value = "";
                    if (file) void onAddImageStamp?.(file);
                  }}
                />
              </>
            )}
            {/* Live Text (OCR) is a reading action: recognised text becomes selectable and
                searchable, so it stays available in Read rather than hiding behind Markup. */}
            {((ocrAvailable && currentPageScanned && onRecognize) ||
              (ocrAvailable && onRecognizeDocument)) && (
              <span className="mx-1 h-5 w-px flex-shrink-0 bg-line" />
            )}
            {ocrAvailable && currentPageScanned && onRecognize && (
              <button
                type="button"
                onClick={onRecognize}
                disabled={
                  ocrState === "running" ||
                  ocrState === "done" ||
                  documentOcrState?.status === "running"
                }
                aria-label={
                  ocrState === "running" ? "Recognizing text" : "Recognize text on this page"
                }
                title={
                  ocrState === "done" ? "Text recognized" : "Recognize text on this scanned page"
                }
                className={capsuleBtn(false)}
              >
                <Scan size={15} weight="regular" aria-hidden />
              </button>
            )}
            {ocrAvailable && onRecognizeDocument && (
              <>
                <button
                  type="button"
                  onClick={onRecognizeDocument}
                  disabled={
                    documentOcrState?.status === "running" ||
                    documentOcrState?.status === "done" ||
                    ocrState === "running"
                  }
                  aria-label={
                    documentOcrState?.status === "running"
                      ? "Live Text — recognizing scanned pages"
                      : "Live Text"
                  }
                  title={
                    documentOcrState?.status === "done"
                      ? "Live Text — all scanned pages recognized"
                      : documentOcrState?.status === "error"
                        ? "Live Text — retry recognising scanned pages"
                        : "Live Text — recognise text on all scanned pages"
                  }
                  className={capsuleBtn(false)}
                >
                  <Scan size={15} weight="regular" aria-hidden />
                </button>
                {documentOcrState && documentOcrState.status === "running" && (
                  <span
                    role="status"
                    aria-live="polite"
                    className="flex-shrink-0 whitespace-nowrap font-mono text-muted-2"
                  >
                    {documentOcrState.completed}/{documentOcrState.total}
                  </span>
                )}
                {documentOcrState && documentOcrState.status === "error" && (
                  <span className="flex-shrink-0 whitespace-nowrap font-mono text-danger">
                    OCR failed
                  </span>
                )}
              </>
            )}
            {/* Zoom group. */}
            <span className="mx-1 h-5 w-px flex-shrink-0 bg-line" />
            <button
              type="button"
              onClick={() => onZoom(-0.15)}
              aria-label="Zoom out"
              title="Zoom out"
              className={capsuleBtn(false)}
            >
              <Minus size={14} weight="regular" aria-hidden />
            </button>
            <input
              type="number"
              value={scaleDraft}
              aria-label="Zoom percentage"
              onChange={(e) => onScaleDraftChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  onCommitScaleDraft();
                  (e.target as HTMLInputElement).blur();
                }
              }}
              onBlur={onCommitScaleDraft}
              className="w-14 flex-shrink-0 rounded-md border border-line bg-ink px-1 py-1 text-center font-mono text-paper outline-none focus:border-copper"
            />
            <span className="flex-shrink-0 font-mono text-muted-2">%</span>
            <button
              type="button"
              onClick={() => onZoom(0.15)}
              aria-label="Zoom in"
              title="Zoom in"
              className={capsuleBtn(false)}
            >
              <Plus size={14} weight="regular" aria-hidden />
            </button>
            <span className="mx-1 h-5 w-px flex-shrink-0 bg-line" />
            <button
              type="button"
              onClick={() => onSetFit("width")}
              aria-pressed={fitMode === "width"}
              aria-label="Fit to width"
              title="Fit to width"
              className={capsuleBtn(fitMode === "width")}
            >
              <IconFitWidth />
            </button>
            <button
              type="button"
              onClick={() => onSetFit("page")}
              aria-pressed={fitMode === "page"}
              aria-label="Fit full page"
              title="Fit full page"
              className={capsuleBtn(fitMode === "page")}
            >
              <IconFitPage />
            </button>
            {/* Rotate group. */}
            <span className="mx-1 h-5 w-px flex-shrink-0 bg-line" />
            <button
              type="button"
              onClick={() => onRotate(-90)}
              aria-label="Rotate left"
              className={capsuleBtn(false)}
              title="Rotate left"
            >
              <ArrowCounterClockwise size={14} weight="regular" aria-hidden />
            </button>
            <button
              type="button"
              onClick={() => onRotate(90)}
              aria-label="Rotate right"
              className={capsuleBtn(false)}
              title="Rotate right"
            >
              <ArrowClockwise size={14} weight="regular" aria-hidden />
            </button>
          </DocumentToolbarShell>
        </div>
        {ocrErrorMessage && (
          <div role="alert" className="vr-capsule px-3 py-1.5 text-xs text-danger">
            {ocrErrorMessage}
          </div>
        )}
        <DocumentToolbarSearch
          findBar={findBar}
          activeMode={findMode}
          onRequestFind={onRequestFind}
          onRequestReplace={onRequestReplace}
        />
      </DocBar>
    </div>
  );
}

function AddedTextSizeInput({
  value,
  onChange,
}: {
  value: number;
  onChange: (value: number) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  return (
    <label className="ml-1 flex flex-shrink-0 items-center gap-1 whitespace-nowrap text-muted-2">
      <span>Size</span>
      <input
        type="number"
        min={6}
        max={144}
        step={1}
        value={draft ?? value}
        aria-label="Added text size"
        onChange={(event) => {
          const next = event.target.value;
          setDraft(next);
          if (next !== "") onChange(Number(next));
        }}
        onBlur={() => setDraft(null)}
        className="w-12 rounded-md border border-line bg-ink px-1 py-1 text-center font-mono text-paper outline-none focus:border-copper"
      />
    </label>
  );
}
