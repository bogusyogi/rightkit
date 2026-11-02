import type { EditorView } from "@codemirror/view";
import type { RefObject, ReactNode } from "react";
import { FullscreenIcon } from "@rightkit/document-core/documents/FullscreenIcon";
import { TextEditor, type SourceEditorViewState } from "../txt/TxtEditor.js";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import type { SourceRange } from "@rightkit/html-editor/documents/html/sourceMap";

/**
 * The in-place HTML source drawer: a resizable pane (splitter + optional fullscreen) hosting the
 * editable CodeMirror source for the active page (entry or a navigated sub-page). Extracted verbatim
 * from HtmlDocument — no behavior change.
 */
export function HtmlDocumentSource({
  active,
  subLoaded,
  activePath,
  activeBuffer,
  activeName,
  activeDirty,
  drawerWidth,
  sourceFullscreen,
  onToggleFullscreen,
  onStartResize,
  onActiveSourceChange,
  onActiveSave,
  onDiscardPage,
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  sourceViewStateRef,
  sourceEditorRef,
  pendingInspectRangeRef,
  revealSourceRange,
}: {
  active: boolean;
  subLoaded: boolean;
  activePath: string;
  activeBuffer: string;
  activeName: string;
  activeDirty: boolean;
  drawerWidth: number;
  sourceFullscreen: boolean;
  onToggleFullscreen: () => void;
  onStartResize: (e: import("react").PointerEvent<HTMLDivElement>) => void;
  onActiveSourceChange: (next: string) => void;
  onActiveSave: () => void;
  onDiscardPage: () => void;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  sourceViewStateRef: RefObject<Record<string, SourceEditorViewState>>;
  sourceEditorRef: RefObject<EditorView | null>;
  pendingInspectRangeRef: RefObject<SourceRange | null>;
  revealSourceRange: (range: SourceRange) => boolean;
}) {
  return (
    <>
      {!sourceFullscreen && (
        <div
          onPointerDown={onStartResize}
          role="separator"
          tabIndex={0}
          aria-orientation="vertical"
          aria-label="Resize source drawer"
          aria-valuemin={280}
          aria-valuenow={Math.round(drawerWidth)}
          className="w-1.5 shrink-0 cursor-col-resize bg-line transition-colors hover:bg-copper/60"
        />
      )}
      <div
        className={`flex min-w-0 flex-col bg-panel ${sourceFullscreen ? "flex-1" : "shrink-0"}`}
        style={sourceFullscreen ? undefined : { width: drawerWidth, maxWidth: "calc(100% - 6px)" }}
      >
        <div className="min-h-0 flex-1">
          {subLoaded ? (
            <TextEditor
              dockToolbar={false}
              active={active}
              key={activePath}
              documentPath={activePath}
              mono
              language="html"
              text={activeBuffer}
              onChange={onActiveSourceChange}
              onSave={onActiveSave}
              registerSearchProvider={registerSearchProvider}
              findBar={findBar}
              findMode={findMode}
              onRequestFind={onRequestFind}
              onRequestReplace={onRequestReplace}
              sourceToolbarActions={
                <>
                  <button
                    type="button"
                    onClick={onToggleFullscreen}
                    className="vr-press vr-focus flex h-6 w-6 items-center justify-center rounded text-muted hover:bg-panel-2 hover:text-paper"
                    aria-label={sourceFullscreen ? "Restore HTML split" : "Expand HTML source"}
                    title={sourceFullscreen ? "Restore split" : "Expand source"}
                  >
                    <FullscreenIcon restore={sourceFullscreen} />
                  </button>
                  <button
                    type="button"
                    onClick={onActiveSave}
                    title={`Save ${activeName}`}
                    className={`vr-press vr-focus rounded px-2 py-1 ${activeDirty ? "text-copper-hi" : "text-muted hover:bg-panel-2 hover:text-paper"}`}
                  >
                    save
                  </button>
                  <button
                    type="button"
                    onClick={onDiscardPage}
                    disabled={!activeDirty}
                    title={`Discard ${activeName}`}
                    className="vr-press vr-focus rounded px-2 py-1 text-muted hover:bg-panel-2 hover:text-paper disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    discard
                  </button>
                </>
              }
              initialViewState={sourceViewStateRef.current[activePath] ?? null}
              onViewStateChange={(state) => {
                sourceViewStateRef.current[activePath] = state;
              }}
              onEditorReady={(view) => {
                sourceEditorRef.current = view;
                if (!view) return;
                const pending = pendingInspectRangeRef.current;
                if (!pending) return;
                // The editor restores its saved scroll in a rAF right after this fires (see
                // TextEditor mount). Reveal on a LATER frame so our scroll-to-match isn't undone —
                // otherwise inspecting from a freshly-opened drawer selects the range off-screen.
                window.requestAnimationFrame(() =>
                  window.requestAnimationFrame(() => {
                    const range = pendingInspectRangeRef.current;
                    if (range && revealSourceRange(range)) pendingInspectRangeRef.current = null;
                  }),
                );
              }}
            />
          ) : (
            <div className="flex h-full items-center justify-center text-xs text-muted-2">
              Loading source…
            </div>
          )}
        </div>
      </div>
    </>
  );
}
