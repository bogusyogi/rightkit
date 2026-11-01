import type { EditorView } from "@codemirror/view";
import type { ReactNode } from "react";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import type { SourceLanguage } from "./sourceFormatting.js";

export interface SourceEditorViewState {
  anchor: number;
  head: number;
  scrollTop: number;
  scrollLeft: number;
}

export type SourceLinkPrefix = "[" | "![";

export interface SourceLinkState {
  start: number;
  end: number;
  text: string;
  prefix: SourceLinkPrefix;
}

export interface TextEditorProps {
  /** Exact owning tab path for resource census attribution. */
  documentPath?: string;
  text: string;
  onChange: (text: string) => void;
  onSave: () => void;
  /** Mono for source/code editing; reading font for plain txt. */
  mono?: boolean;
  /** Show the markdown formatting bar (txt + md/mdx source). Parity with new Notepad / md editors. */
  markdownToolbar?: boolean;
  /** Read-only source view, e.g. navigated HTML sub-pages that are not the entry file. */
  readOnly?: boolean;
  /** Source language profile. Markdown gets toolbar-aware shortcuts; HTML gets HTML parsing; plain is for TXT. */
  language?: SourceLanguage;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  /** Shared FindBar element (or null), floated top-right inside the editor area. */
  findBar?: ReactNode;
  /** Put the source toolbar in the app's docked document bar (default). Embedded sources that
   *  share the bar with another capsule (HTML's source drawer) keep their own strip. */
  dockToolbar?: boolean;
  /** Optional right-side controls rendered in the source toolbar. */
  sourceToolbarActions?: ReactNode;
  initialViewState?: SourceEditorViewState | null;
  onViewStateChange?: (state: SourceEditorViewState) => void;
  onEditorReady?: (view: EditorView | null) => void;
  onSourceScrollLine?: (line: number) => void;
  autoFocus?: boolean;
  sourceFocusMode?: boolean;
  sourceTypewriterMode?: boolean;
  /** Use document-level undo/redo instead of CodeMirror history. */
  externalHistory?: boolean;
}
