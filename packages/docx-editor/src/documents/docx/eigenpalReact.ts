// Bridge to the published @eigenpal/docx-editor-react. The host-owned toolbar (`renderToolbar`)
// is a small patch to DocxEditor that this package carries in patches/ until it lands upstream;
// the types below describe that patched contract. Against an unpatched editor the prop is
// ignored and the editor keeps its own toolbar.
import type { ForwardRefExoticComponent, ReactNode, RefAttributes } from "react";
import {
  DocxEditor as BaseDocxEditor,
  type DocxEditorProps as BaseDocxEditorProps,
  type DocxEditorRef,
  type EditorMode,
} from "@eigenpal/docx-editor-react";
import type { ColorValue, ParagraphAlignment, Style, Theme } from "@eigenpal/docx-editor-core/types/document";

export type { DocxEditorRef, EditorMode };

/** List state of the paragraph under the selection (mirrors the editor's internal ListState). */
export interface DocxListState {
  type: "none" | "bullet" | "numbered" | (string & {});
  level: number;
  isInList: boolean;
  numId?: number;
}

/** Formatting of the current selection (mirrors the editor's SelectionFormatting). */
export interface SelectionFormatting {
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  superscript?: boolean;
  subscript?: boolean;
  fontFamily?: string;
  fontSize?: number;
  color?: string;
  highlight?: string;
  alignment?: ParagraphAlignment;
  listState?: DocxListState;
  lineSpacing?: number;
  styleId?: string;
  indentLeft?: number;
  bidi?: boolean;
}

/** Formatting commands the editor accepts (mirrors the editor's FormattingAction). */
export type FormattingAction =
  | "bold"
  | "italic"
  | "underline"
  | "strikethrough"
  | "superscript"
  | "subscript"
  | "clearFormatting"
  | "bulletList"
  | "numberedList"
  | "indent"
  | "outdent"
  | "insertLink"
  | "setRtl"
  | "setLtr"
  | { type: "fontFamily"; value: string }
  | { type: "fontSize"; value: number }
  | { type: "textColor"; value: ColorValue | string }
  | { type: "highlightColor"; value: string }
  | { type: "alignment"; value: ParagraphAlignment }
  | { type: "lineSpacing"; value: number }
  | { type: "applyStyle"; value: string };

/**
 * Host-owned toolbar bridge: the host renders its own DOCX toolbar while the editor keeps the
 * document engine, layout, dialogs, command plumbing and selection restoration.
 */
export interface DocxHostToolbarContext {
  selectionFormatting: SelectionFormatting;
  disabled: boolean;
  canUndo: boolean;
  canRedo: boolean;
  editingMode: EditorMode;
  zoom: number;
  format: (action: FormattingAction) => void;
  undo: () => void;
  redo: () => void;
  save: () => void | Promise<void>;
  print: () => void;
  open: () => void;
  setEditingMode: (mode: EditorMode) => void;
  setZoom: (zoom: number) => void;
  refocusEditor: () => void;
  insertTable: (rows: number, columns: number) => void;
  insertImage: () => void;
  insertPageBreak: () => void;
  insertTOC: () => void;
  pageSetup: () => void;
  documentStyles: Style[] | undefined;
  theme: Theme | null | undefined;
}

export type DocxEditorProps = BaseDocxEditorProps & {
  /** Host-owned full toolbar renderer (patched editor; see patches/). */
  renderToolbar?: (context: DocxHostToolbarContext) => ReactNode;
};

export const DocxEditor = BaseDocxEditor as unknown as ForwardRefExoticComponent<
  DocxEditorProps & RefAttributes<DocxEditorRef>
>;
