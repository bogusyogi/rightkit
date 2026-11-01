import {
  defaultKeymap,
  historyKeymap,
  indentLess,
  indentMore,
  toggleComment,
} from "@codemirror/commands";
import { autocompletion, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import type { Completion, CompletionContext } from "@codemirror/autocomplete";
import { MARKDOWN_INSERT_TEMPLATES } from "@rightkit/markdown-editor/documents/markdown/insert/markdownInsertTemplates";
import { foldGutter, foldKeymap } from "@codemirror/language";
import { linter, lintKeymap } from "@codemirror/lint";
import {
  Decoration,
  EditorView,
  gutter,
  GutterMarker,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  ViewPlugin,
  WidgetType,
  type BlockInfo,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import {
  EditorSelection,
  EditorState,
  Prec,
  RangeSetBuilder,
  StateEffect,
  StateField,
  type Extension,
} from "@codemirror/state";
import type { MutableRefObject } from "react";
import type { MarkdownDocumentCore } from "@rightkit/markdown-editor/documents/markdown/core/MarkdownDocumentCore";
import type { TextMatch } from "@rightkit/document-core/documents/searchUtils";
import { buildImageEmbed, sourceDiagnostics, type SourceLanguage } from "./sourceFormatting";

export const setSearchHighlights = StateEffect.define<{
  matches: TextMatch[];
  activeIndex: number;
}>();

export const searchHighlightField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, tr) {
    let next = value.map(tr.changes);
    for (const effect of tr.effects) {
      if (effect.is(setSearchHighlights))
        next = buildSearchDecorations(effect.value.matches, effect.value.activeIndex);
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

export const searchHighlightClickExtension = Prec.highest(
  EditorView.domEventHandlers({
    mousedown(event, view) {
      if (event.button !== 0) return false;
      const eventTarget = event.target;
      const target =
        eventTarget instanceof Element
          ? eventTarget
          : eventTarget instanceof Node
            ? eventTarget.parentElement
            : null;
      if (!target?.closest(".cm-vr-searchMatchActive")) return false;
      // Find already owns the exact match selection. Run before CodeMirror's pointer handler so one
      // click transfers focus without first collapsing or relocating that exact range.
      event.preventDefault();
      view.focus();
      return true;
    },
  }),
);

class InvisibleCharWidget extends WidgetType {
  constructor(private readonly label: string) {
    super();
  }
  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-vr-invisibleChar";
    span.textContent = this.label;
    return span;
  }
}

// T2-4: decorate only the visible ranges (view.visibleRanges), not the whole document. The prior
// StateField called doc.toString() + looped every character on every docChanged with no windowing \u2014
// O(document) per keystroke regardless of file size. A ViewPlugin recomputes only on doc/viewport
// change and only walks what's actually on screen (plus CodeMirror's small overdraw margin).
class InvisibleCharsPlugin {
  decorations: DecorationSet;
  constructor(view: EditorView) {
    this.decorations = buildInvisibleDecorations(view);
  }
  update(update: ViewUpdate) {
    if (update.docChanged || update.viewportChanged) {
      this.decorations = buildInvisibleDecorations(update.view);
    }
  }
}

export function invisibleCharactersExtension(enabled: boolean): Extension[] {
  if (!enabled) return [];
  return [
    ViewPlugin.fromClass(InvisibleCharsPlugin, {
      decorations: (plugin) => plugin.decorations,
    }),
  ];
}

function buildInvisibleDecorations(view: EditorView): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  for (const { from, to } of view.visibleRanges) {
    const text = view.state.sliceDoc(from, to);
    for (let i = 0; i < text.length; i++) {
      const char = text[i];
      const pos = from + i;
      if (char === " ")
        builder.add(
          pos,
          pos + 1,
          Decoration.replace({ widget: new InvisibleCharWidget("\u00b7") }),
        );
      else if (char === "\t")
        builder.add(
          pos,
          pos + 1,
          Decoration.replace({ widget: new InvisibleCharWidget("\u2192") }),
        );
    }
  }
  return builder.finish();
}

function buildSearchDecorations(matches: TextMatch[], activeIndex: number): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  matches.forEach((match, index) => {
    if (match.end <= match.start) return;
    builder.add(
      match.start,
      match.end,
      Decoration.mark({
        class:
          index === activeIndex ? "cm-vr-searchMatch cm-vr-searchMatchActive" : "cm-vr-searchMatch",
      }),
    );
  });
  return builder.finish();
}

export function languageExtension(_language: SourceLanguage): Extension[] {
  return [];
}

export async function loadLanguageExtension(language: SourceLanguage): Promise<Extension[]> {
  if (language === "markdown") {
    const [{ markdown }, { javascript: jsLang }, { LanguageDescription }] = await Promise.all([
      import("@codemirror/lang-markdown"),
      import("@codemirror/lang-javascript"),
      import("@codemirror/language"),
    ]);
    return [
      markdown({
        codeLanguages: [
          LanguageDescription.of({
            name: "javascript",
            alias: ["js", "jsx", "mjs", "cjs"],
            support: jsLang(),
          }),
          LanguageDescription.of({
            name: "typescript",
            alias: ["ts", "tsx"],
            support: jsLang({ typescript: true, jsx: true }),
          }),
        ],
      }),
    ];
  }
  if (language === "html") {
    const { html } = await import("@codemirror/lang-html");
    return [html({ autoCloseTags: true, selfClosingTags: true })];
  }
  return [];
}

export function visualSourceLineSelection(view: EditorView, pos: number) {
  const cursor = EditorSelection.cursor(pos);
  return {
    anchor: view.moveToLineBoundary(cursor, false, true).head,
    head: view.moveToLineBoundary(cursor, true, true).head,
  };
}

function selectSourceLine(view: EditorView, line: BlockInfo, event: MouseEvent) {
  const contentRect = view.contentDOM.getBoundingClientRect();
  const hit = view.posAtCoords({ x: contentRect.left + 1, y: event.clientY }, false);
  const pos = Math.max(line.from, Math.min(line.to, hit ?? line.from));
  view.dispatch({
    selection: visualSourceLineSelection(view, pos),
    scrollIntoView: true,
    userEvent: "select.pointer",
  });
  view.focus();
}

function selectLineFromGutter(view: EditorView, line: BlockInfo, event: Event): boolean {
  if (!(event instanceof MouseEvent) || event.button !== 0) return false;
  const target = event.target instanceof Element ? event.target : null;
  if (target?.closest(".cm-foldGutter")) return false;
  selectSourceLine(view, line, event);
  event.preventDefault();
  return true;
}

class LineSelectMarker extends GutterMarker {
  toDOM(): Node {
    const span = document.createElement("span");
    span.setAttribute("aria-hidden", "true");
    return span;
  }
}

const lineSelectMarker = new LineSelectMarker();
const plainLineSelectGutter = gutter({
  class: "cm-lineSelectGutter",
  lineMarker: () => lineSelectMarker,
  domEventHandlers: {
    mousedown: selectLineFromGutter,
  },
});

export function gutterExtension(language: SourceLanguage): Extension[] {
  if (language === "plain") return [plainLineSelectGutter];
  return [
    lineNumbers({ domEventHandlers: { mousedown: selectLineFromGutter } }),
    foldGutter(),
    highlightActiveLineGutter(),
  ];
}

export function readOnlyExtension(readOnly: boolean): Extension[] {
  return [EditorState.readOnly.of(readOnly), EditorView.editable.of(!readOnly)];
}

export function contentAttributesExtension(spellcheck: boolean, readOnly: boolean): Extension {
  return EditorView.contentAttributes.of({
    spellcheck: String(spellcheck && !readOnly),
    "aria-label": "Source editor",
  });
}

function wordWrapExtension(wordWrap: boolean): Extension[] {
  return wordWrap
    ? [
        EditorView.lineWrapping,
        EditorView.theme({
          ".cm-scroller": { overflowX: "hidden !important" },
          ".cm-content": { minWidth: "0 !important" },
        }),
      ]
    : [];
}

export function sourceWrapExtension(
  wordWrap: boolean,
  language: SourceLanguage,
  sourceWrap: boolean,
): Extension[] {
  return language === "plain" ? wordWrapExtension(wordWrap) : wordWrapExtension(sourceWrap);
}

export function imagePasteExtension(
  language: SourceLanguage,
  onMarkdownHtmlPaste?: (event: ClipboardEvent, view: EditorView) => boolean | Promise<boolean>,
): Extension[] {
  if (language === "plain") return [];
  return [
    EditorView.domEventHandlers({
      paste(event, view) {
        if (
          language === "markdown" &&
          onMarkdownHtmlPaste &&
          event.clipboardData?.getData("text/html")
        ) {
          const handled = onMarkdownHtmlPaste(event, view);
          if (handled instanceof Promise) {
            void handled;
            return true;
          }
          if (handled) return true;
        }
        const items = event.clipboardData?.items;
        if (!items) return false;
        const image = Array.from(items).find(
          (it) => it.kind === "file" && it.type.startsWith("image/"),
        );
        const file = image?.getAsFile();
        if (!file) return false;
        event.preventDefault();
        const reader = new FileReader();
        reader.onload = () => {
          const dataUrl = typeof reader.result === "string" ? reader.result : "";
          if (!dataUrl) return;
          const snippet = buildImageEmbed(language, dataUrl);
          const { from, to } = view.state.selection.main;
          view.dispatch({
            changes: { from, to, insert: snippet },
            selection: { anchor: from + snippet.length },
            scrollIntoView: true,
          });
        };
        reader.readAsDataURL(file);
        return true;
      },
    }),
  ];
}

export function diagnosticsExtension(
  language: SourceLanguage,
  _core: MarkdownDocumentCore | null = null,
): Extension[] {
  if (language === "plain") return [];
  return [linter((view) => sourceDiagnostics(language, view.state.doc.toString()), { delay: 350 })];
}

export async function loadDiagnosticsExtension(
  language: SourceLanguage,
  core: MarkdownDocumentCore | null = null,
): Promise<Extension[]> {
  if (language === "plain") return [];
  if (language !== "markdown" || !core) {
    return diagnosticsExtension(language, core);
  }
  const [{ analyzeMarkdown }, { buildCodeMirrorDiagnostics }] = await Promise.all([
    import("@rightkit/markdown-editor/documents/markdown/analysis/analyzeMarkdown"),
    import("@rightkit/markdown-editor/documents/markdown/analysis/markdownIssueCodeMirror"),
  ]);
  return [
    linter(
      (view) => {
        const diagnostics = sourceDiagnostics(language, view.state.doc.toString());
        const snapshot = core.snapshot();
        if (snapshot.logicalSource !== view.state.doc.toString()) return diagnostics;
        return [...diagnostics, ...buildCodeMirrorDiagnostics(analyzeMarkdown(snapshot))];
      },
      { delay: 350 },
    ),
  ];
}

/** Caret-anchored "/" insert menu for Markdown/MDX source: CodeMirror's own completion popup
 *  (filter-as-you-type, Esc closes, Enter inserts) over the shared insert templates. Triggers on
 *  "/" at line start or after whitespace; the "/query" text is replaced in one transaction. */
export function slashInsertCompletionSource(context: CompletionContext) {
  const match = context.matchBefore(/(?:^|\s)\/[\w-]*/);
  if (!match) return null;
  const slashAt = match.from + match.text.indexOf("/");
  const options: Completion[] = MARKDOWN_INSERT_TEMPLATES.map((template) => ({
    label: template.label,
    detail: "/ insert",
    type: "text",
    apply: (view, _completion, _from, to) => {
      view.dispatch({
        changes: { from: slashAt, to, insert: template.snippet },
        selection: { anchor: slashAt + template.cursorOffset },
        userEvent: "input.complete",
      });
    },
  }));
  return { from: slashAt + 1, options, validFor: /^[\w-]*$/ };
}

export function completionExtension(language: SourceLanguage): Extension {
  return autocompletion({
    ...(language === "markdown" ? { override: [slashInsertCompletionSource] } : {}),
    activateOnTyping: language !== "plain",
    defaultKeymap: false,
    icons: false,
  });
}

export async function loadMarkdownCoreCompletionExtension(
  core: MarkdownDocumentCore | null,
): Promise<Extension[]> {
  if (!core) return [];
  const { completionStartAt, completionsAt } = await import(
    "@rightkit/markdown-editor/documents/markdown/references/markdownCompletion"
  );
  return [
    autocompletion({
      override: [
        slashInsertCompletionSource,
        (context: CompletionContext) => {
          const before = context.state.sliceDoc(0, context.pos);
          const from = completionStartAt(before);
          if (from === null) return null;
          const snapshot = core.snapshot();
          const options = completionsAt({
            textBefore: before,
            folderFiles: [],
            sessionFiles: [],
            headings: snapshot.sourceMap.nodes,
          });
          if (!options.length) return null;
          return {
            from,
            options: options.map((option) => ({
              label: option.label,
              detail: option.detail,
              type: option.kind === "image" ? "text" : "link",
              apply: option.insertText,
            })),
          };
        },
      ],
      activateOnTyping: true,
      defaultKeymap: false,
      icons: false,
    }),
  ];
}

export function sourceKeymap(
  markdownToolbar: boolean,
  onSave: () => void,
  onFormat: () => void,
  allowNextTabFocusRef: MutableRefObject<boolean>,
  wrapInline: (before: string, after: string) => void,
  openLink: (prefix: "[" | "![") => void,
  externalHistory = false,
  externalUndo?: () => void,
  externalRedo?: () => void,
): Extension {
  return keymap.of([
    ...(externalHistory
      ? [
          {
            key: "Mod-z",
            run: () => {
              externalUndo?.();
              return true;
            },
          },
          {
            key: "Mod-y",
            run: () => {
              externalRedo?.();
              return true;
            },
          },
          {
            key: "Mod-Shift-z",
            run: () => {
              externalRedo?.();
              return true;
            },
          },
        ]
      : []),
    {
      key: "Mod-s",
      run: () => {
        onSave();
        return true;
      },
    },
    {
      key: "Shift-Alt-f",
      run: () => {
        onFormat();
        return true;
      },
    },
    { key: "Mod-/", run: toggleComment },
    {
      key: "Escape",
      run: () => {
        allowNextTabFocusRef.current = true;
        return false;
      },
    },
    {
      key: "Tab",
      run: (view) => {
        if (allowNextTabFocusRef.current) {
          allowNextTabFocusRef.current = false;
          return false;
        }
        return indentMore(view);
      },
    },
    { key: "Shift-Tab", run: indentLess },
    ...(markdownToolbar
      ? [
          {
            key: "Mod-b",
            run: () => {
              wrapInline("**", "**");
              return true;
            },
          },
          {
            key: "Mod-i",
            run: () => {
              wrapInline("*", "*");
              return true;
            },
          },
          {
            key: "Mod-k",
            run: () => {
              openLink("[");
              return true;
            },
          },
        ]
      : []),
    // Completion keys first: with the slash/completion popup open, Enter accepts the option instead
    // of defaultKeymap's insertNewlineAndIndent (completionKeymap commands return false when closed).
    ...completionKeymap,
    ...defaultKeymap,
    ...(externalHistory ? [] : historyKeymap),
    ...closeBracketsKeymap,
    ...lintKeymap,
    ...foldKeymap,
  ]);
}
