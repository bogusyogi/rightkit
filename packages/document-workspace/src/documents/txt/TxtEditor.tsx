import { lazy, Suspense, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { EditorView } from "@codemirror/view";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { LinkDialog } from "./LinkDialog.js";
import type { SourceLanguage } from "./sourceFormatting.js";
export {
  applyInlineWrap,
  buildImageEmbed,
  buildMarkdownLink,
  formatHtmlSource,
  formatSource,
  sourceDiagnostics,
} from "./sourceFormatting.js";
import { viewRightTheme } from "./sourceTheme.js";
import { SourceEditorToolbar } from "./SourceEditorToolbar.js";
import { DocumentToolbarSearch, type DocumentSearchMode } from "@rightkit/document-core/documents/DocumentToolbarFind";
import { useSourceSearch } from "./useSourceSearch.js";
import { useTextEditorCommands } from "./useTextEditorCommands.js";
import { useCodeMirrorSourceEditor } from "./useCodeMirrorSourceEditor.js";
import { printDocumentContent } from "@rightkit/document-core/app/documentPrinting";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { toast } from "@rightkit/platform-ui/headless";
import type { SourceEditorViewState, SourceLinkState, TextEditorProps } from "./sourceEditorTypes.js";
import { DocBar } from "@rightkit/document-core/app/shell/docBar";
export type { SourceEditorViewState } from "./sourceEditorTypes.js";

const MarkdownSourceToolbar = lazy(() =>
  import("./MarkdownSourceToolbar.js").then((m) => ({ default: m.MarkdownSourceToolbar })),
);

interface Props extends TextEditorProps {
  text: string;
  onChange: (text: string) => void;
  onSave: () => void;
  /** Mono for source/code editing; reading font for plain txt. */
  mono?: boolean;
  /** Show the markdown formatting bar (txt + md/mdx source). Parity with new Notepad / md editors. */
  markdownToolbar?: boolean;
  /** Read-only source view — no editing or replace (e.g. navigated HTML sub-pages that aren't the entry file). */
  readOnly?: boolean;
  /** Source language profile. Markdown gets toolbar-aware shortcuts; HTML gets HTML parsing; plain is for TXT. */
  language?: SourceLanguage;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  /** Shared FindBar element placed in source toolbar chrome. */
  findBar?: ReactNode;
  findMode?: DocumentSearchMode;
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  /** Optional right-side controls rendered in the source toolbar. */
  sourceToolbarActions?: ReactNode;
  initialViewState?: SourceEditorViewState | null;
  onViewStateChange?: (state: SourceEditorViewState) => void;
  onEditorReady?: (view: EditorView | null) => void;
  onSourceScrollLine?: (line: number) => void;
  autoFocus?: boolean;
  sourceFocusMode?: boolean;
  sourceTypewriterMode?: boolean;
  active?: boolean;
  markdownCore?: import("@rightkit/markdown-editor/documents/markdown/core/MarkdownDocumentCore").MarkdownDocumentCore;
}

export function TextEditor({
  documentPath,
  text,
  onChange,
  onSave,
  mono = false,
  markdownToolbar = false,
  readOnly = false,
  dockToolbar = true,
  language = markdownToolbar ? "markdown" : "plain",
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  sourceToolbarActions,
  initialViewState = null,
  onViewStateChange,
  onEditorReady,
  onSourceScrollLine,
  autoFocus = false,
  sourceFocusMode = false,
  sourceTypewriterMode = false,
  externalHistory = false,
  active = false,
  markdownCore: explicitMarkdownCore,
}: Props) {
  const markdownCore = explicitMarkdownCore ?? null;
  const coreHistory = language === "markdown" ? markdownCore : null;
  const { settings } = useSettings();
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const activeRef = useRef(active);
  activeRef.current = active;

  useEffect(() => {
    if (!active || language !== "plain") return;
    const onPrint = (event: Event) => {
      event.preventDefault();
      void printDocumentContent({ kind: "plain-text", text: textRef.current })
        .then((printed) => {
          if (printed) return;
          logError("print:failed:txt");
          toast("Couldn't open print dialog", "error");
        })
        .catch(() => {
          logError("print:failed:txt");
          toast("Couldn't open print dialog", "error");
        });
    };
    window.addEventListener("viewright:print", onPrint);
    return () => window.removeEventListener("viewright:print", onPrint);
  }, [active, language]);
  const allowNextTabFocusRef = useRef(false);
  const suppressChangeRef = useRef(false);
  const textRef = useRef(text);
  const onChangeRef = useRef(onChange);
  const onSaveRef = useRef(onSave);
  const formatOnSaveRef = useRef(false);
  const onViewStateChangeRef = useRef(onViewStateChange);
  const onEditorReadyRef = useRef(onEditorReady);
  const onSourceScrollLineRef = useRef(onSourceScrollLine);
  const readOnlyRef = useRef(readOnly);
  const registerSearchProviderRef = useRef(registerSearchProvider);
  const registeredProviderRef = useRef<SearchProvider | null>(null);
  const sourceWrap = true;
  const [sourceFolded, setSourceFolded] = useState(false);
  const [showInvisibles, setShowInvisibles] = useState(false);
  const [goLineOpen, setGoLineOpen] = useState(false);
  const [goLineValue, setGoLineValue] = useState("");
  const useReadingFont = language === "plain" && !mono;
  // Memoize the theme so the SAME extension instance flows into both the initial `themeCompartment.of`
  // and the reconfigure effect. Rebuilding it per render churned CodeMirror's StyleModule (each
  // EditorView.theme() mints a new generated class, and the editor could end up tagged with a class
  // whose styles were already unmounted) — which silently dropped the source font-size/padding, so the
  // editor fell back to the inherited 16px instead of var(--source-size). That nondeterministic size is
  // the "font shift" seen on opening source.
  const themeExtension = useMemo(
    () => viewRightTheme(language, useReadingFont),
    [language, useReadingFont],
  );
  // Link/image insertion via the shared Notepad-style dialog (Display text + Address).
  const [linkState, setLinkState] = useState<SourceLinkState | null>(null);

  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);
  useEffect(() => {
    onSaveRef.current = onSave;
  }, [onSave]);
  useEffect(() => {
    formatOnSaveRef.current = settings.formatOnSave;
  }, [settings.formatOnSave]);
  useEffect(() => {
    onViewStateChangeRef.current = onViewStateChange;
  }, [onViewStateChange]);
  useEffect(() => {
    onEditorReadyRef.current = onEditorReady;
  }, [onEditorReady]);
  useEffect(() => {
    onSourceScrollLineRef.current = onSourceScrollLine;
  }, [onSourceScrollLine]);
  const {
    dispatchSearchHighlights,
    refreshSearch,
    replaceAndFind,
    runFind,
    searchRef,
    stepSearch,
  } = useSourceSearch({ viewRef, readOnlyRef });

  const {
    formatFromEditor,
    formatTablesFromEditor,
    goToLine,
    insertBlock,
    insertLinkFromDialog: applyLinkFromDialog,
    linePrefix,
    openLink,
    saveFromEditor,
    setHeading,
    toggleFoldAll,
    wrapInline,
  } = useTextEditorCommands({
    formatOnSaveRef,
    language,
    onSaveRef,
    readOnlyRef,
    setLinkState,
    setSourceFolded,
    sourceFolded,
    viewRef,
  });

  const insertLinkFromDialog = (label: string, url: string) => {
    const st = linkState;
    setLinkState(null);
    applyLinkFromDialog(st, label, url);
  };

  useCodeMirrorSourceEditor({
    activeRef,
    allowNextTabFocusRef,
    autoFocus,
    dispatchSearchHighlights,
    documentPath,
    externalHistory: externalHistory || Boolean(coreHistory),
    formatFromEditor,
    hostRef,
    initialViewState,
    language,
    markdownToolbar,
    onChangeRef,
    onEditorReadyRef,
    onSourceScrollLineRef,
    onViewStateChangeRef,
    openLink,
    readOnly,
    readOnlyRef,
    registerSearchProvider,
    registerSearchProviderRef,
    registeredProviderRef,
    refreshSearch,
    replaceAndFind,
    runFind,
    saveFromEditor,
    searchRef,
    settingsSpellcheck: settings.spellcheck,
    settingsWordWrap: settings.wordWrap,
    showInvisibles,
    sourceTypewriterMode,
    sourceWrap,
    stepSearch,
    suppressChangeRef,
    text,
    textRef,
    themeExtension,
    viewRef,
    wrapInline,
    markdownCore: coreHistory,
  });

  // Inactive panes are hidden with `content-visibility: hidden`; when one is shown again CodeMirror
  // re-measures from estimated line heights and lands on the wrong offset. Remember the offset the
  // user left while active and put it back after the pane is visible again.
  const activeScrollRef = useRef<{ top: number; left: number } | null>(null);
  useEffect(() => {
    const scroller = viewRef.current?.scrollDOM;
    if (!active || !scroller) return;
    const saved = activeScrollRef.current;
    const restore = () => {
      if (!saved) return;
      scroller.scrollTop = saved.top;
      scroller.scrollLeft = saved.left;
    };
    let frame = requestAnimationFrame(() => {
      restore();
      frame = requestAnimationFrame(restore);
    });
    const onScroll = () => {
      activeScrollRef.current = { top: scroller.scrollTop, left: scroller.scrollLeft };
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      scroller.removeEventListener("scroll", onScroll);
    };
  }, [active]);

  return (
    <>
      {linkState && (
        <LinkDialog
          initialText={linkState.text}
          onInsert={insertLinkFromDialog}
          onCancel={() => setLinkState(null)}
        />
      )}
      <div className="relative flex h-full min-w-0 flex-col overflow-hidden">
        {language !== "plain" && dockToolbar && (
          <DocBar>
            <div className="vr-capsule h-[42px] min-w-0 px-1">
              <div className="vr-document-toolbar-search-layout flex min-w-0 items-center">
                <div className="vr-document-toolbar-main min-w-0">
                  <SourceEditorToolbar
                    folded={sourceFolded}
                    showInvisibles={showInvisibles}
                    actions={sourceToolbarActions}
                    markdownControls={
                      markdownToolbar ? (
                        <Suspense fallback={null}>
                          <MarkdownSourceToolbar
                            setHeading={setHeading}
                            wrapInline={wrapInline}
                            linePrefix={linePrefix}
                            openLink={openLink}
                            insertBlock={insertBlock}
                            formatTables={formatTablesFromEditor}
                          />
                        </Suspense>
                      ) : undefined
                    }
                    onFormat={formatFromEditor}
                    onToggleFold={toggleFoldAll}
                    onGoToLine={() => setGoLineOpen(true)}
                    onToggleInvisibles={() => setShowInvisibles((value) => !value)}
                  />
                </div>
                <DocumentToolbarSearch
                  findBar={findBar}
                  activeMode={findMode}
                  onRequestFind={onRequestFind}
                  onRequestReplace={onRequestReplace}
                />
              </div>
            </div>
          </DocBar>
        )}
        {language !== "plain" && !dockToolbar && (
          <div className="no-print vr-document-toolbar flex shrink-0 flex-col">
            <div className="vr-document-toolbar-search-layout">
              <div className="vr-document-toolbar-main min-w-0">
                <SourceEditorToolbar
                  folded={sourceFolded}
                  showInvisibles={showInvisibles}
                  actions={sourceToolbarActions}
                  markdownControls={
                    markdownToolbar ? (
                      <Suspense fallback={null}>
                        <MarkdownSourceToolbar
                          setHeading={setHeading}
                          wrapInline={wrapInline}
                          linePrefix={linePrefix}
                          openLink={openLink}
                          insertBlock={insertBlock}
                          formatTables={formatTablesFromEditor}
                        />
                      </Suspense>
                    ) : undefined
                  }
                  onFormat={formatFromEditor}
                  onToggleFold={toggleFoldAll}
                  onGoToLine={() => setGoLineOpen(true)}
                  onToggleInvisibles={() => setShowInvisibles((value) => !value)}
                />
              </div>
              <DocumentToolbarSearch
                findBar={findBar}
                activeMode={findMode}
                onRequestFind={onRequestFind}
                onRequestReplace={onRequestReplace}
              />
            </div>
          </div>
        )}
        <div className="relative min-h-0 min-w-0 flex-1 overflow-hidden">
          {language === "plain" && findBar && (
            <div className="no-print vr-chrome absolute right-3 top-3 z-50 max-w-[calc(100%-1.5rem)]">
              {findBar}
            </div>
          )}
          {goLineOpen && (
            <form
              className="absolute right-3 top-3 z-50 flex items-center gap-2 rounded border border-line bg-panel-2 px-2 py-1 text-chrome-xs text-muted shadow-xl"
              onSubmit={(e) => {
                e.preventDefault();
                const line = Number.parseInt(goLineValue, 10);
                if (Number.isFinite(line)) goToLine(line);
                setGoLineOpen(false);
                setGoLineValue("");
              }}
            >
              <label htmlFor="vr-source-go-line">line</label>
              <input
                id="vr-source-go-line"
                value={goLineValue}
                onChange={(e) => setGoLineValue(e.target.value)}
                className="w-20 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
                inputMode="numeric"
              />
              <button
                type="submit"
                className="vr-press vr-focus rounded px-2 py-1 text-copper-hi hover:bg-panel"
              >
                go
              </button>
              <button
                type="button"
                className="vr-press vr-focus rounded px-2 py-1 hover:bg-panel hover:text-paper"
                onClick={() => {
                  setGoLineOpen(false);
                  setGoLineValue("");
                }}
              >
                close
              </button>
            </form>
          )}
          {/* data-vr-density drives the editor font-size/leading/padding via stable global CSS
              (styles/index.css). CodeMirror's per-instance EditorView.theme injection proved flaky
              for these size-critical props — its generated class could be applied while its StyleModule
              was unmounted, dropping the source font-size back to the inherited 16px. */}
          <div
            ref={hostRef}
            data-vr-density={useReadingFont ? "reading" : language === "plain" ? "plain" : "source"}
            className={`h-full w-full min-w-0 ${sourceFocusMode ? "vr-source-focus" : ""}`}
          />
        </div>
      </div>
    </>
  );
}
