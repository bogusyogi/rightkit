import { EditorView } from "@codemirror/view";
import type { MarkdownDocumentCore } from "./core/MarkdownDocumentCore";
import { createInitialMarkdownSourceMap } from "./core/buildMarkdownSourceMap";
import { MarkdownNodeIdFactory } from "./core/MarkdownNodeIdFactory";
import type { EditorView as MarkdownEditorView } from "prosemirror-view";

// Read-only oracle for compiled-app QA. User edits still travel through native editor input.
const cores = new Map<string, MarkdownDocumentCore>();
const views = new Map<string, MarkdownEditorView>();
export function observeQaMarkdownView(path: string, view: MarkdownEditorView): () => void {
  views.set(path, view);
  return () => {
    if (views.get(path) === view) views.delete(path);
  };
}

declare global {
  interface Window {
    __viewrightQaMarkdown?: {
      snapshot: (path: string) => ReturnType<MarkdownDocumentCore["snapshot"]> | null;
      freshMap: (path: string) => ReturnType<typeof createInitialMarkdownSourceMap> | null;
      documents: () => string[];
      sourceSelection: () => { anchor: number; head: number; length: number } | null;
      renderedSelection: (path: string) => unknown;
    };
  }
}

export function observeQaMarkdownCore(path: string, core: MarkdownDocumentCore): () => void {
  cores.set(path, core);
  window.__viewrightQaMarkdown ??= Object.freeze({
    snapshot: (documentPath: string) => cores.get(documentPath)?.snapshot() ?? null,
    freshMap: (documentPath: string) => {
      const snapshot = cores.get(documentPath)?.snapshot();
      return snapshot
        ? createInitialMarkdownSourceMap(
            snapshot.logicalSource,
            snapshot.revision,
            new MarkdownNodeIdFactory("qa-fresh-map"),
          )
        : null;
    },
    documents: () => [...cores.keys()],
    renderedSelection: (path: string) => {
      const view = views.get(path);
      return view && !view.isDestroyed
        ? {
            selection: view.state.selection.toJSON(),
            documentSize: view.state.doc.content.size,
            composing: view.composing,
            plugins: view.state.plugins.map((plugin) => ({
              keyboard: !!plugin.props.handleKeyDown,
            })),
          }
        : null;
    },
    sourceSelection: () => {
      const node = document.querySelector('[data-vr-pane-state="active"] .cm-content');
      const view = node ? EditorView.findFromDOM(node as HTMLElement) : null;
      return view
        ? {
            anchor: view.state.selection.main.anchor,
            head: view.state.selection.main.head,
            length: view.state.doc.length,
          }
        : null;
    },
  });
  return () => {
    if (cores.get(path) === core) cores.delete(path);
  };
}
