import { useEffect, useMemo, useRef, type MutableRefObject } from "react";
import { closeBrackets } from "@codemirror/autocomplete";
import { history, redo, undo } from "@codemirror/commands";
import { bracketMatching, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches } from "@codemirror/search";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import {
  dropCursor,
  EditorView,
  highlightActiveLine,
  rectangularSelection,
} from "@codemirror/view";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import type { TextMatch } from "@rightkit/document-core/documents/searchUtils";
import type { SourceLanguage } from "./sourceFormatting";
import type { SourceEditorViewState, SourceLinkPrefix } from "./sourceEditorTypes";
import type { MarkdownDocumentCore } from "@rightkit/markdown-editor/documents/markdown/core/MarkdownDocumentCore";
import { bytesToBase64 } from "@rightkit/document-core/documents/base64";
import {
  completionExtension,
  contentAttributesExtension,
  diagnosticsExtension,
  gutterExtension,
  imagePasteExtension,
  invisibleCharactersExtension,
  languageExtension,
  loadDiagnosticsExtension,
  loadLanguageExtension,
  loadMarkdownCoreCompletionExtension,
  readOnlyExtension,
  searchHighlightClickExtension,
  searchHighlightField,
  sourceKeymap,
  sourceWrapExtension,
} from "./sourceExtensions";
import { sourceHighlightStyle } from "./sourceTheme";
import { acquireResource } from "@rightkit/document-core/app/resourceCensus";

interface UseCodeMirrorSourceEditorArgs {
  activeRef: MutableRefObject<boolean>;
  allowNextTabFocusRef: MutableRefObject<boolean>;
  autoFocus: boolean;
  dispatchSearchHighlights: (matches: TextMatch[], activeIndex: number) => void;
  documentPath?: string;
  externalHistory: boolean;
  formatFromEditor: () => void;
  hostRef: MutableRefObject<HTMLDivElement | null>;
  initialViewState: SourceEditorViewState | null;
  language: SourceLanguage;
  markdownToolbar: boolean;
  markdownCore: MarkdownDocumentCore | null;
  onChangeRef: MutableRefObject<(text: string) => void>;
  onEditorReadyRef: MutableRefObject<((view: EditorView | null) => void) | undefined>;
  onSourceScrollLineRef: MutableRefObject<((line: number) => void) | undefined>;
  onViewStateChangeRef: MutableRefObject<((state: SourceEditorViewState) => void) | undefined>;
  openLink: (prefix: SourceLinkPrefix) => void;
  readOnly: boolean;
  readOnlyRef: MutableRefObject<boolean>;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  registerSearchProviderRef: MutableRefObject<(provider: SearchProvider | null) => void>;
  registeredProviderRef: MutableRefObject<SearchProvider | null>;
  refreshSearch: () => { count: number; activeIndex: number };
  replaceAndFind: (replacement: string, all: boolean) => { count: number; activeIndex: number };
  runFind: SearchProvider["find"];
  saveFromEditor: () => void;
  searchRef: MutableRefObject<{
    query: string;
    options: { matchCase: boolean; wholeWord: boolean; regex?: boolean };
    matches: TextMatch[];
    activeIndex: number;
  }>;
  settingsSpellcheck: boolean;
  settingsWordWrap: boolean;
  showInvisibles: boolean;
  sourceTypewriterMode: boolean;
  sourceWrap: boolean;
  stepSearch: (delta: number) => { count: number; activeIndex: number };
  suppressChangeRef: MutableRefObject<boolean>;
  text: string;
  textRef: MutableRefObject<string>;
  themeExtension: Extension;
  viewRef: MutableRefObject<EditorView | null>;
  wrapInline: (before: string, after: string) => void;
}

export function useCodeMirrorSourceEditor(args: UseCodeMirrorSourceEditorArgs) {
  // The EditorView construction effect is mount-only. Runtime settings live in compartments below;
  // putting every changing argument in its dependency list destroys/recreates the view on a simple
  // read-only or spellcheck update. Keep the initial construction snapshot separate from live refs.
  const constructionArgsRef = useRef(args);
  constructionArgsRef.current = args;
  const languageCompartment = useMemo(() => new Compartment(), []);
  const readOnlyCompartment = useMemo(() => new Compartment(), []);
  const contentAttributesCompartment = useMemo(() => new Compartment(), []);
  const wordWrapCompartment = useMemo(() => new Compartment(), []);
  const gutterCompartment = useMemo(() => new Compartment(), []);
  const themeCompartment = useMemo(() => new Compartment(), []);
  const keymapCompartment = useMemo(() => new Compartment(), []);
  const completionCompartment = useMemo(() => new Compartment(), []);
  const diagnosticsCompartment = useMemo(() => new Compartment(), []);
  const invisiblesCompartment = useMemo(() => new Compartment(), []);

  useEffect(() => {
    const previousRegister = args.registerSearchProviderRef.current;
    if (previousRegister === args.registerSearchProvider) return;
    args.registerSearchProviderRef.current = args.registerSearchProvider;
    const provider = args.registeredProviderRef.current;
    if (!provider) return;
    previousRegister(null);
    args.registerSearchProvider(provider);
  }, [args.registerSearchProvider, args.registerSearchProviderRef, args.registeredProviderRef]);

  useEffect(() => {
    const args = constructionArgsRef.current;
    const host = args.hostRef.current;
    if (!host) return;
    let applyingFromCore = false;
    const handleSmartPaste = async (event: ClipboardEvent, view: EditorView) => {
      const core = args.markdownCore;
      if (!core || args.language !== "markdown") return false;
      const html = event.clipboardData?.getData("text/html") ?? "";
      const text = event.clipboardData?.getData("text/plain") ?? "";
      if (!html) return false;
      event.preventDefault();
      const [
        { smartPaste },
        { DEFAULT_MARKDOWN_ASSET_SETTINGS },
        { NativeMarkdownAssetClient, validateMarkdownAsset },
      ] = await Promise.all([
        import("@rightkit/markdown-editor/documents/markdown/assets/smartPaste"),
        import("@rightkit/markdown-editor/documents/markdown/assets/markdownAssetSettings"),
        import("@rightkit/markdown-editor/documents/markdown/assets/markdownAsset"),
      ]);
      const selection = view.state.selection.main;
      const snapshot = core.snapshot();
      const client = new NativeMarkdownAssetClient();
      const saved: string[] = [];
      try {
        const result = await smartPaste({
          html,
          text,
          documentBasename: core.state.documentId.split(/[\\/]/).pop() ?? "document",
          settings: DEFAULT_MARKDOWN_ASSET_SETTINGS,
          insertAt: selection.from,
          resolveAssetPath: async (payload) => {
            const error = validateMarkdownAsset(payload);
            if (error) throw error;
            const source = assetPayloadBase64(payload.source);
            const savedAsset = await client.save({
              payloadBase64: source,
              hintName: payload.hintName,
              mimeKind: payload.mimeKind,
              documentPath: core.state.documentId,
            });
            if (savedAsset.rollbackToken) saved.push(savedAsset.rollbackToken);
            return savedAsset.relativePath;
          },
        });
        const edits = result.edits.map((edit) => ({ ...edit, to: selection.to }));
        core.applyCommand({
          edits,
          expectedRevision: snapshot.revision,
          origin: "source",
          label: "Paste",
          isolateHistory: true,
        });
        await Promise.all(saved.map((token) => client.commit(token)));
      } catch {
        await Promise.all(saved.map((token) => client.rollback(token)));
      }
      return true;
    };
    const extensions: Extension[] = [
      ...(args.externalHistory ? [] : [history()]),
      dropCursor(),
      rectangularSelection(),
      highlightActiveLine(),
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      completionCompartment.of(completionExtension(args.language)),
      syntaxHighlighting(sourceHighlightStyle, { fallback: true }),
      highlightSelectionMatches(),
      searchHighlightField,
      searchHighlightClickExtension,
      gutterCompartment.of(gutterExtension(args.language)),
      diagnosticsCompartment.of(diagnosticsExtension(args.language, null)),
      themeCompartment.of(args.themeExtension),
      readOnlyCompartment.of(readOnlyExtension(args.readOnly)),
      contentAttributesCompartment.of(
        contentAttributesExtension(args.settingsSpellcheck, args.readOnly),
      ),
      wordWrapCompartment.of(
        sourceWrapExtension(args.settingsWordWrap, args.language, args.sourceWrap),
      ),
      invisiblesCompartment.of(invisibleCharactersExtension(args.showInvisibles)),
      imagePasteExtension(args.language, args.markdownCore ? handleSmartPaste : undefined),
      EditorView.updateListener.of((update) => {
        const core = args.markdownCore;
        if (update.selectionSet && core && args.language === "markdown") {
          const selection = update.state.selection.main;
          core.postSelectionHint({
            surface: "source",
            nodeId: null,
            anchor: selection.anchor,
            head: selection.head,
            assoc: selection.head >= selection.anchor ? -1 : 1,
          });
        }
        if (update.selectionSet && args.sourceTypewriterMode) {
          const head = update.state.selection.main.head;
          update.view.dispatch({ effects: EditorView.scrollIntoView(head, { y: "center" }) });
        }
        if (!update.docChanged) return;
        // Feed CodeMirror's change set directly to the core to avoid
        // the whole-string LCS diff on every keystroke. This keeps the
        // hot path O(edits) instead of O(n*m) for large documents.
        const next = update.state.doc.toString();
        args.textRef.current = next;
        if (args.suppressChangeRef.current || applyingFromCore) return;
        if (!core) {
          args.onChangeRef.current(next);
          return;
        }
        const snapshot = core.snapshot();
        if (snapshot.logicalSource === next) return;
        // Prefer the incremental change set; fall back to whole-doc diff
        // only for external reconciliation where the guard applies.
        const edits: import("@rightkit/markdown-editor/documents/markdown/core/types").MarkdownTextEdit[] = [];
        let hasChanges = false;
        try {
          update.changes.iterChanges(
            (
              fromA: number,
              toA: number,
              _fromB: number,
              _toB: number,
              inserted: import("@codemirror/state").Text,
            ) => {
              hasChanges = true;
              edits.push({
                from: fromA,
                to: toA,
                insert: inserted.toString(),
                inheritMode: "inherit" as const,
              });
            },
          );
        } catch {
          // If iterating fails, fall back to bounded diff (external path).
          hasChanges = false;
        }
        if (hasChanges) {
          try {
            core.applyCommand({
              edits,
              expectedRevision: snapshot.revision,
              origin: "source",
              label: "Typing",
              coalesceKey: "source:typing",
            });
            return;
          } catch {
            // fall through to recovery — stale revision or validation
          }
        } else {
          // CodeMirror normally yields at least one range for docChanged. If
          // it does not, keep source typing incremental without invoking LCS.
          try {
            core.applyCommand({
              edits: [
                {
                  from: 0,
                  to: snapshot.logicalSource.length,
                  insert: next,
                  inheritMode: "preserve" as const,
                },
              ],
              expectedRevision: snapshot.revision,
              origin: "source",
              label: "Typing",
              coalesceKey: "source:typing",
            });
            return;
          } catch {
            // fall through to recovery — stale revision or validation
          }
        }
        // Recovery: resync editor from core.
        applyingFromCore = true;
        try {
          const current = core.snapshot().logicalSource;
          view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: current } });
          args.textRef.current = current;
        } finally {
          applyingFromCore = false;
        }
      }),
      keymapCompartment.of(
        sourceKeymap(
          args.markdownToolbar,
          args.saveFromEditor,
          args.formatFromEditor,
          args.allowNextTabFocusRef,
          args.wrapInline,
          args.openLink,
          args.externalHistory,
          () => args.markdownCore?.undo(),
          () => args.markdownCore?.redo(),
        ),
      ),
      languageCompartment.of(languageExtension(args.language)),
    ];

    const view = new EditorView({
      state: EditorState.create({
        doc: args.textRef.current,
        selection: args.initialViewState
          ? {
              anchor: Math.min(args.initialViewState.anchor, args.textRef.current.length),
              head: Math.min(args.initialViewState.head, args.textRef.current.length),
            }
          : undefined,
        extensions,
      }),
      parent: host,
    });
    args.viewRef.current = view;
    const releaseCensus = acquireResource({
      kind: "codemirror-view",
      documentPath: args.markdownCore?.state.documentId ?? args.documentPath,
      format: args.language === "markdown" ? "md" : args.language === "html" ? "html" : "txt",
    });
    // MarkdownSurfaceId.CodeMirrorSource === 1 << 1 === 2. Avoid static import of markdown core enum to keep TXT isolated.
    const MARKDOWN_SURFACE_ID_CODEMIRROR = 2;
    const searchResultListeners = new Set<
      (query: string, result: { count: number; activeIndex: number }) => void
    >();
    const unsubscribeCore = args.markdownCore?.subscribe(
      MARKDOWN_SURFACE_ID_CODEMIRROR as unknown as Parameters<
        NonNullable<typeof args.markdownCore>["subscribe"]
      >[0],
      (summary: import("@rightkit/markdown-editor/documents/markdown/core/MarkdownDocumentTypes").MarkdownCommitSummary) => {
        const next = args.markdownCore?.snapshot().logicalSource;
        if (next !== undefined && view.state.doc.toString() !== next) {
          applyingFromCore = true;
          try {
            const changes = summary.forwardEdits.map((edit) => ({
              from: edit.from,
              to: edit.to,
              insert: edit.insert,
            }));
            const changeSet = view.state.changes(changes);
            const transaction = view.state.update({
              changes: changeSet,
              selection: mapSelectionThroughChanges(view.state.selection.main, changeSet),
            });
            if (changes.length && transaction.newDoc.toString() === next) {
              view.dispatch(transaction);
            } else {
              const { from, to, insert } = boundedSourceTransition(view.state.doc.toString(), next);
              const fallbackChanges = view.state.changes({ from, to, insert });
              view.dispatch({
                changes: fallbackChanges,
                selection: mapSelectionThroughChanges(view.state.selection.main, fallbackChanges),
              });
            }
            args.textRef.current = next;
          } finally {
            applyingFromCore = false;
          }
          if (args.searchRef.current.query) {
            const result = args.refreshSearch();
            const query = args.searchRef.current.query;
            for (const listener of searchResultListeners) listener(query, result);
          }
        }
        const hint = summary.selectionHint;
        if (hint?.surface === "prosemirror") {
          const anchor = Math.max(0, Math.min(hint.anchor, view.state.doc.length));
          const head = Math.max(0, Math.min(hint.head, view.state.doc.length));
          view.dispatch({ selection: { anchor, head } });
        }
      },
    );
    args.onEditorReadyRef.current?.(view);
    if (args.autoFocus && !args.readOnlyRef.current) {
      requestAnimationFrame(() => {
        if (args.viewRef.current === view) view.focus();
      });
    }
    const onSourceScroll = () => {
      const block = view.lineBlockAtHeight(view.scrollDOM.scrollTop + 12);
      args.onSourceScrollLineRef.current?.(view.state.doc.lineAt(block.from).number);
    };
    view.scrollDOM.addEventListener("scroll", onSourceScroll, { passive: true });

    const onUndo = () => {
      if (args.activeRef.current) undo(view);
    };
    const onRedo = () => {
      if (args.activeRef.current) redo(view);
    };
    if (!args.externalHistory) {
      window.addEventListener("viewright:undo", onUndo);
      window.addEventListener("viewright:redo", onRedo);
    }
    requestAnimationFrame(() => {
      if (args.viewRef.current !== view) return;
      view.requestMeasure();
      requestAnimationFrame(() => {
        if (args.viewRef.current === view) view.requestMeasure();
      });
    });
    if (args.initialViewState) {
      requestAnimationFrame(() => {
        if (args.viewRef.current === view && args.initialViewState) {
          view.scrollDOM.scrollTop = args.initialViewState.scrollTop;
          view.scrollDOM.scrollLeft = args.initialViewState.scrollLeft;
        }
      });
    }

    const provider: SearchProvider = {
      kind: "source",
      canReplace: !args.readOnlyRef.current,
      supportsMatchCase: true,
      supportsWholeWord: true,
      supportsRegex: true,
      find: (query, options) => args.runFind(query, options),
      next: () => args.stepSearch(1),
      prev: () => args.stepSearch(-1),
      replace: (replacement) => args.replaceAndFind(replacement, false),
      replaceAll: (replacement) => args.replaceAndFind(replacement, true),
      subscribe: (listener) => {
        searchResultListeners.add(listener);
        return () => searchResultListeners.delete(listener);
      },
      close: () => {
        args.searchRef.current = {
          query: "",
          options: args.searchRef.current.options,
          matches: [],
          activeIndex: -1,
        };
        args.dispatchSearchHighlights([], -1);
      },
    };
    args.registeredProviderRef.current = provider;
    args.registerSearchProviderRef.current(provider);

    return () => {
      unsubscribeCore?.();
      if (!args.externalHistory) {
        window.removeEventListener("viewright:undo", onUndo);
        window.removeEventListener("viewright:redo", onRedo);
      }
      view.scrollDOM.removeEventListener("scroll", onSourceScroll);
      const sel = view.state.selection.main;
      args.onViewStateChangeRef.current?.({
        anchor: sel.anchor,
        head: sel.head,
        scrollTop: view.scrollDOM.scrollTop,
        scrollLeft: view.scrollDOM.scrollLeft,
      });
      args.registerSearchProviderRef.current(null);
      args.registeredProviderRef.current = null;
      args.onEditorReadyRef.current?.(null);
      view.destroy();
      releaseCensus();
      if (args.viewRef.current === view) args.viewRef.current = null;
    };
  }, [
    completionCompartment,
    contentAttributesCompartment,
    diagnosticsCompartment,
    gutterCompartment,
    invisiblesCompartment,
    keymapCompartment,
    languageCompartment,
    readOnlyCompartment,
    themeCompartment,
    wordWrapCompartment,
  ]);

  useEffect(() => {
    const view = args.viewRef.current;
    if (!view) return;
    if (args.language === "plain") {
      view.dispatch({
        effects: [
          languageCompartment.reconfigure(languageExtension(args.language)),
          gutterCompartment.reconfigure(gutterExtension(args.language)),
          completionCompartment.reconfigure(completionExtension(args.language)),
          diagnosticsCompartment.reconfigure(diagnosticsExtension(args.language, null)),
        ],
      });
      return;
    }
    let cancelled = false;
    void (async () => {
      const [langExt, diagExt] = await Promise.all([
        loadLanguageExtension(args.language),
        loadDiagnosticsExtension(args.language, args.markdownCore),
      ]);
      if (cancelled || args.viewRef.current !== view) return;
      let combinedCompletion: Extension = completionExtension(args.language);
      if (args.language === "markdown" && args.markdownCore) {
        const mdCompletion = await loadMarkdownCoreCompletionExtension(args.markdownCore);
        if (cancelled || args.viewRef.current !== view) return;
        combinedCompletion = mdCompletion.length
          ? mdCompletion // includes the slash insert source; a second override would conflict
          : combinedCompletion;
      }
      view.dispatch({
        effects: [
          languageCompartment.reconfigure(langExt),
          gutterCompartment.reconfigure(gutterExtension(args.language)),
          completionCompartment.reconfigure(combinedCompletion),
          diagnosticsCompartment.reconfigure(diagExt),
        ],
      });
    })();
    return () => {
      cancelled = true;
    };
  }, [
    args.language,
    args.markdownCore,
    args.viewRef,
    completionCompartment,
    diagnosticsCompartment,
    gutterCompartment,
    languageCompartment,
  ]);

  useEffect(() => {
    args.viewRef.current?.dispatch({ effects: themeCompartment.reconfigure(args.themeExtension) });
  }, [args.themeExtension, args.viewRef, themeCompartment]);

  useEffect(() => {
    args.readOnlyRef.current = args.readOnly;
    args.registeredProviderRef.current = args.registeredProviderRef.current
      ? { ...args.registeredProviderRef.current, canReplace: !args.readOnly }
      : null;
    if (args.registeredProviderRef.current)
      args.registerSearchProviderRef.current(args.registeredProviderRef.current);

    const view = args.viewRef.current;
    if (!view) return;
    view.dispatch({
      effects: [
        readOnlyCompartment.reconfigure(readOnlyExtension(args.readOnly)),
        contentAttributesCompartment.reconfigure(
          contentAttributesExtension(args.settingsSpellcheck, args.readOnly),
        ),
      ],
    });
  }, [
    args.readOnly,
    args.readOnlyRef,
    args.registerSearchProviderRef,
    args.registeredProviderRef,
    args.settingsSpellcheck,
    args.viewRef,
    contentAttributesCompartment,
    readOnlyCompartment,
  ]);

  useEffect(() => {
    args.viewRef.current?.dispatch({
      effects: wordWrapCompartment.reconfigure(
        sourceWrapExtension(args.settingsWordWrap, args.language, args.sourceWrap),
      ),
    });
  }, [args.language, args.settingsWordWrap, args.sourceWrap, args.viewRef, wordWrapCompartment]);

  useEffect(() => {
    args.viewRef.current?.dispatch({
      effects: invisiblesCompartment.reconfigure(invisibleCharactersExtension(args.showInvisibles)),
    });
  }, [args.showInvisibles, args.viewRef, invisiblesCompartment]);

  useEffect(() => {
    args.viewRef.current?.dispatch({
      effects: keymapCompartment.reconfigure(
        sourceKeymap(
          args.markdownToolbar,
          args.saveFromEditor,
          args.formatFromEditor,
          args.allowNextTabFocusRef,
          args.wrapInline,
          args.openLink,
          args.externalHistory,
          () => args.markdownCore?.undo(),
          () => args.markdownCore?.redo(),
        ),
      ),
    });
  }, [
    args.allowNextTabFocusRef,
    args.externalHistory,
    args.formatFromEditor,
    args.markdownToolbar,
    args.markdownCore,
    args.openLink,
    args.saveFromEditor,
    args.viewRef,
    args.wrapInline,
    keymapCompartment,
  ]);

  useEffect(() => {
    const view = args.viewRef.current;
    if (!view) return;
    if (args.textRef.current === args.text && view.state.doc.toString() === args.text) return;
    args.suppressChangeRef.current = true;
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: args.text } });
    args.suppressChangeRef.current = false;
    args.textRef.current = args.text;
    if (args.searchRef.current.query)
      args.runFind(args.searchRef.current.query, args.searchRef.current.options);
  }, [args.runFind, args.searchRef, args.suppressChangeRef, args.text, args.textRef, args.viewRef]);
}

function mapSelectionThroughChanges(
  selection: import("@codemirror/state").SelectionRange,
  changes: import("@codemirror/state").ChangeSet,
) {
  const forward = selection.anchor <= selection.head;
  const anchorAssoc = selection.empty ? 1 : forward ? -1 : 1;
  const headAssoc = selection.empty ? 1 : forward ? 1 : -1;
  return {
    anchor: changes.mapPos(selection.anchor, anchorAssoc),
    head: changes.mapPos(selection.head, headAssoc),
  };
}

function boundedSourceTransition(previous: string, next: string) {
  let from = 0;
  const commonLimit = Math.min(previous.length, next.length);
  while (from < commonLimit && previous.charCodeAt(from) === next.charCodeAt(from)) from++;
  let previousEnd = previous.length;
  let nextEnd = next.length;
  while (
    previousEnd > from &&
    nextEnd > from &&
    previous.charCodeAt(previousEnd - 1) === next.charCodeAt(nextEnd - 1)
  ) {
    previousEnd--;
    nextEnd--;
  }
  return { from, to: previousEnd, insert: next.slice(from, nextEnd) };
}

function assetPayloadBase64(source: Uint8Array | string): string {
  if (typeof source !== "string") return bytesToBase64(source);
  const comma = source.indexOf(",");
  return source.startsWith("data:") && comma >= 0 ? source.slice(comma + 1) : source;
}
