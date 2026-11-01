import { useCallback, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import { foldAll, unfoldAll } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import {
  applyInlineWrap,
  buildMarkdownLink,
  formatSource,
  type SourceLanguage,
} from "./sourceFormatting";
import type { SourceLinkPrefix, SourceLinkState } from "./sourceEditorTypes";

export function useTextEditorCommands({
  formatOnSaveRef,
  language,
  onSaveRef,
  readOnlyRef,
  setLinkState,
  setSourceFolded,
  sourceFolded,
  viewRef,
}: {
  formatOnSaveRef: MutableRefObject<boolean>;
  language: SourceLanguage;
  onSaveRef: MutableRefObject<() => void>;
  readOnlyRef: MutableRefObject<boolean>;
  setLinkState: Dispatch<SetStateAction<SourceLinkState | null>>;
  setSourceFolded: Dispatch<SetStateAction<boolean>>;
  sourceFolded: boolean;
  viewRef: MutableRefObject<EditorView | null>;
}) {
  const edit = useCallback(
    (
      fn: (
        value: string,
        start: number,
        end: number,
      ) => { value: string; start: number; end: number },
    ) => {
      const view = viewRef.current;
      if (!view || readOnlyRef.current) return;
      const sel = view.state.selection.main;
      const result = fn(view.state.doc.toString(), sel.from, sel.to);
      view.focus();
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: result.value },
        selection: { anchor: result.start, head: result.end },
        userEvent: "input",
      });
    },
    [readOnlyRef, viewRef],
  );

  const wrapInline = useCallback(
    (before: string, after: string) =>
      edit((value, start, end) => applyInlineWrap(value, start, end, before, after)),
    [edit],
  );

  const openLink = useCallback(
    (prefix: SourceLinkPrefix) => {
      const view = viewRef.current;
      if (!view || readOnlyRef.current) return;
      const sel = view.state.selection.main;
      setLinkState({
        start: sel.from,
        end: sel.to,
        text: view.state.sliceDoc(sel.from, sel.to),
        prefix,
      });
    },
    [readOnlyRef, setLinkState, viewRef],
  );

  const formatFromEditor = useCallback(() => {
    const view = viewRef.current;
    if (!view || readOnlyRef.current) return;
    const current = view.state.doc.toString();
    const next = formatSource(language, current);
    if (next === current) return;
    view.focus();
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: next },
      selection: { anchor: Math.min(view.state.selection.main.head, next.length) },
      userEvent: "input.format",
    });
  }, [language, readOnlyRef, viewRef]);

  const formatTablesFromEditor = useCallback(() => {
    const view = viewRef.current;
    if (!view || readOnlyRef.current || language !== "markdown") return;
    const current = view.state.doc.toString();
    void import("@rightkit/markdown-editor/documents/markdown/markdownTools").then(({ formatMarkdownTables }) => {
      const next = formatMarkdownTables(current);
      if (next === current) return;
      const liveView = viewRef.current;
      if (!liveView) return;
      liveView.focus();
      liveView.dispatch({
        changes: { from: 0, to: liveView.state.doc.length, insert: next },
        selection: { anchor: Math.min(liveView.state.selection.main.head, next.length) },
        userEvent: "input.format",
      });
    });
  }, [language, readOnlyRef, viewRef]);

  const saveFromEditor = useCallback(() => {
    if (formatOnSaveRef.current && language !== "plain") formatFromEditor();
    onSaveRef.current();
  }, [formatFromEditor, formatOnSaveRef, language, onSaveRef]);

  const toggleFoldAll = useCallback(() => {
    const view = viewRef.current;
    if (!view) return;
    if (sourceFolded) unfoldAll(view);
    else foldAll(view);
    setSourceFolded((value) => !value);
  }, [setSourceFolded, sourceFolded, viewRef]);

  const goToLine = useCallback(
    (lineNumber: number) => {
      const view = viewRef.current;
      if (!view) return;
      const line = view.state.doc.line(Math.max(1, Math.min(lineNumber, view.state.doc.lines)));
      view.focus();
      view.dispatch({
        selection: { anchor: line.from },
        effects: EditorView.scrollIntoView(line.from, { y: "center" }),
      });
    },
    [viewRef],
  );

  const insertLinkFromDialog = (state: SourceLinkState | null, label: string, url: string) => {
    const view = viewRef.current;
    if (!view || !state || readOnlyRef.current) return;
    const md = buildMarkdownLink(state.prefix, label, url);
    view.focus();
    view.dispatch({
      changes: { from: state.start, to: state.end, insert: md },
      selection: { anchor: state.start + md.length },
      userEvent: "input",
    });
  };

  const linePrefix = (prefix: string) =>
    edit((value, start, end) => {
      const lineStart = value.lastIndexOf("\n", start - 1) + 1;
      return {
        value: value.slice(0, lineStart) + prefix + value.slice(lineStart),
        start: start + prefix.length,
        end: end + prefix.length,
      };
    });

  const setHeading = (level: number) =>
    edit((value, start, end) => {
      const lineStart = value.lastIndexOf("\n", start - 1) + 1;
      const nextLine = value.indexOf("\n", start);
      const lineEnd = nextLine === -1 ? value.length : nextLine;
      const line = value.slice(lineStart, lineEnd);
      const stripped = line.replace(/^#{1,6}\s+/, "");
      const prefix = level === 0 ? "" : `${"#".repeat(level)} `;
      const nextLineText = prefix + stripped;
      const delta = nextLineText.length - line.length;
      return {
        value: value.slice(0, lineStart) + nextLineText + value.slice(lineEnd),
        start: start + delta,
        end: end + delta,
      };
    });

  const insertBlock = (block: string, caretOffset = block.length) =>
    edit((value, start) => {
      const lineStart = value.lastIndexOf("\n", start - 1) + 1;
      const prefix = start === lineStart ? "" : "\n";
      const insert = prefix + block;
      return {
        value: value.slice(0, start) + insert + value.slice(start),
        start: start + prefix.length + caretOffset,
        end: start + prefix.length + caretOffset,
      };
    });

  return {
    formatFromEditor,
    formatTablesFromEditor,
    goToLine,
    insertBlock,
    insertLinkFromDialog,
    linePrefix,
    openLink,
    saveFromEditor,
    setHeading,
    toggleFoldAll,
    wrapInline,
  };
}
