import { HighlightStyle } from "@codemirror/language";
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import type { SourceLanguage } from "./sourceFormatting";

const LINE_SELECT_CURSOR =
  'url("data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27 width=%2724%27 height=%2724%27 viewBox=%270 0 24 24%27%3E%3Cg transform=%27matrix(-1 0 0 1 24 0)%27%3E%3Cpath d=%27M5 3l10 9h-5l3 7-2.4 1-3-7L5 17V3z%27 fill=%27white%27 stroke=%27black%27 stroke-width=%271.5%27 stroke-linejoin=%27round%27/%3E%3C/g%3E%3C/svg%3E") 12 4, e-resize';

const sourceTone = {
  text: "var(--paper)",
  tag: "color-mix(in srgb, var(--copper) 76%, var(--paper) 24%)",
  heading1: "color-mix(in srgb, var(--copper) 70%, var(--paper) 30%)",
  heading2: "color-mix(in srgb, var(--copper) 62%, var(--paper) 38%)",
  heading: "color-mix(in srgb, var(--copper) 58%, var(--paper) 42%)",
  list: "color-mix(in srgb, var(--accent-2) 58%, var(--paper) 42%)",
  quote: "color-mix(in srgb, var(--copper-hi) 58%, var(--paper) 42%)",
  attribute: "color-mix(in srgb, var(--accent-2) 64%, var(--paper) 36%)",
  value: "color-mix(in srgb, var(--accent-2) 52%, var(--paper) 48%)",
  punctuation: "color-mix(in srgb, var(--muted) 72%, var(--paper) 28%)",
  comment: "color-mix(in srgb, var(--muted-2) 70%, var(--paper) 30%)",
  literal: "color-mix(in srgb, var(--copper-hi) 68%, var(--paper) 32%)",
  code: "color-mix(in srgb, var(--paper) 86%, var(--accent-2) 14%)",
};

export const sourceHighlightStyle = HighlightStyle.define([
  { tag: [t.content], color: sourceTone.text },
  { tag: [t.heading1], color: sourceTone.heading1, fontWeight: "700" },
  { tag: [t.heading2], color: sourceTone.heading2, fontWeight: "700" },
  {
    tag: [t.heading3, t.heading4, t.heading5, t.heading6, t.heading],
    color: sourceTone.heading,
    fontWeight: "700",
  },
  { tag: [t.tagName, t.keyword, t.operatorKeyword], color: sourceTone.tag },
  { tag: [t.attributeName, t.propertyName, t.variableName], color: sourceTone.attribute },
  { tag: [t.string, t.attributeValue, t.url], color: sourceTone.value },
  { tag: [t.number, t.atom], color: sourceTone.literal },
  { tag: [t.comment, t.docComment], color: sourceTone.comment, fontStyle: "italic" },
  { tag: [t.angleBracket, t.operator, t.punctuation, t.separator], color: sourceTone.punctuation },
  { tag: t.list, color: sourceTone.list },
  { tag: t.quote, color: sourceTone.quote, fontStyle: "italic" },
  {
    tag: t.link,
    color: sourceTone.heading2,
    textDecoration: "underline",
    textUnderlineOffset: "2px",
  },
  {
    tag: t.monospace,
    color: sourceTone.code,
    backgroundColor: "color-mix(in srgb, var(--panel-2) 72%, transparent)",
  },
  { tag: t.strong, color: sourceTone.heading2, fontWeight: "700" },
  { tag: t.emphasis, color: sourceTone.quote, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
]);

function editorDensity(language: SourceLanguage, useReadingFont: boolean) {
  // Plain text has no toolbar above it (unlike Markdown/source), so the first line would otherwise
  // butt right against the window titlebar. Give it a generous top inset to clear the titlebar.
  if (language === "plain" && useReadingFont) {
    return {
      fontSize: "var(--doc-size)",
      lineHeight: "var(--doc-leading)",
      contentPadding: "32px 20px 72px",
    };
  }
  return language === "plain"
    ? { fontSize: "13px", lineHeight: "1.58", contentPadding: "32px 20px 72px" }
    : { fontSize: "var(--source-size)", lineHeight: "1.46", contentPadding: "10px 14px 72px 14px" };
}

export function viewRightTheme(language: SourceLanguage, useReadingFont: boolean): Extension {
  const density = editorDensity(language, useReadingFont);
  const textFont = useReadingFont ? "var(--doc-font)" : "var(--font-mono)";
  const textColor = useReadingFont ? "var(--doc-fg)" : "var(--paper)";
  return EditorView.theme({
    "&": {
      height: "100%",
      backgroundColor: "transparent",
      color: textColor,
      fontSize: density.fontSize,
      letterSpacing: "0",
    },
    "&.cm-focused": {
      outline: "none",
    },
    ".cm-scroller": {
      fontFamily: textFont,
      fontSize: density.fontSize,
      lineHeight: density.lineHeight,
      overflow: "auto",
      textAlign: "left",
      textRendering: "optimizeLegibility",
    },
    ".cm-content": {
      minHeight: "100%",
      padding: density.contentPadding,
      caretColor: "var(--copper)",
      tabSize: "2",
    },
    ".cm-line": {
      padding: "0",
      color: textColor,
    },
    ".cm-gutters": {
      backgroundColor:
        language === "plain"
          ? "transparent"
          : "color-mix(in srgb, var(--panel) 72%, var(--ink) 28%)",
      color: "color-mix(in srgb, var(--muted) 82%, var(--paper) 18%)",
      borderRight:
        language === "plain" ? "0px" : "1px solid color-mix(in srgb, var(--line) 78%, transparent)",
      flexShrink: "0",
      fontSize: "11px",
    },
    ".cm-gutter": {
      minWidth: "0",
    },
    ".cm-lineNumbers": {
      minWidth: "0",
    },
    ".cm-lineSelectGutter": {
      width: "1.15rem",
      minWidth: "1.15rem",
      backgroundColor: "transparent",
    },
    ".cm-lineSelectGutter .cm-gutterElement, .cm-lineNumbers .cm-gutterElement": {
      cursor: LINE_SELECT_CURSOR,
      userSelect: "none",
    },
    ".cm-lineNumbers .cm-gutterElement": {
      boxSizing: "border-box",
      minWidth: "0",
      padding: "0 0.3rem 0 0.25rem",
      fontVariantNumeric: "tabular-nums",
    },
    ".cm-foldGutter": {
      minWidth: "0",
    },
    ".cm-foldGutter .cm-gutterElement": {
      boxSizing: "border-box",
      minWidth: "0",
      width: "0.7rem",
      color: "var(--muted)",
      padding: "0",
    },
    ".cm-activeLine": {
      backgroundColor: "color-mix(in srgb, var(--copper) 7%, transparent)",
    },
    ".cm-activeLineGutter": {
      backgroundColor: "color-mix(in srgb, var(--copper) 9%, transparent)",
      color: "var(--copper)",
    },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
      backgroundColor: "color-mix(in srgb, var(--copper) 32%, transparent)",
    },
    ".cm-cursor": {
      borderLeftColor: "var(--copper)",
    },
    ".cm-vr-searchMatch": {
      backgroundColor: "color-mix(in srgb, var(--copper) 42%, transparent)",
      outline: "1px solid color-mix(in srgb, var(--copper) 60%, transparent)",
      borderRadius: "2px",
    },
    ".cm-vr-searchMatchActive": {
      backgroundColor: "color-mix(in srgb, var(--copper) 78%, transparent)",
      outline: "1.5px solid var(--copper-hi)",
    },
    ".cm-vr-invisibleChar": {
      color: "color-mix(in srgb, var(--muted-2) 76%, transparent)",
    },
    ".cm-diagnostic": {
      backgroundColor: "var(--panel-2)",
      color: "var(--paper)",
      borderColor: "var(--line)",
      fontFamily: "var(--font-mono)",
      fontSize: "12px",
    },
    ".cm-lintRange-warning": {
      backgroundImage:
        "linear-gradient(45deg, transparent 65%, var(--copper-hi) 65% 80%, transparent 80%)",
    },
    ".cm-lintRange-info": {
      backgroundImage:
        "linear-gradient(45deg, transparent 65%, var(--accent-2) 65% 80%, transparent 80%)",
    },
    ".cm-lint-marker-warning, .cm-lint-marker-info": {
      opacity: "0.86",
    },
    ".cm-matchingBracket": {
      backgroundColor: "color-mix(in srgb, var(--accent-2) 18%, transparent)",
      outline: "1px solid color-mix(in srgb, var(--accent-2) 42%, transparent)",
    },
    ".cm-nonmatchingBracket": {
      backgroundColor: "color-mix(in srgb, #b3402a 22%, transparent)",
      outline: "1px solid color-mix(in srgb, #b3402a 58%, transparent)",
    },
    ".cm-dropCursor": {
      borderLeftColor: "var(--copper-hi)",
    },
    ".cm-foldPlaceholder": {
      backgroundColor: "color-mix(in srgb, var(--panel-2) 72%, transparent)",
      borderColor: "var(--line)",
      color: "var(--paper)",
    },
    ".cm-panels, .cm-tooltip": {
      backgroundColor: "var(--panel-2)",
      color: "var(--paper)",
      borderColor: "var(--line)",
    },
    ".cm-tooltip-autocomplete": {
      borderRadius: "6px",
      boxShadow: "0 14px 34px color-mix(in srgb, #000 28%, transparent)",
      overflow: "hidden",
    },
    ".cm-tooltip-autocomplete ul": {
      fontFamily: "var(--font-mono)",
      fontSize: "12px",
      padding: "4px",
    },
    ".cm-tooltip-autocomplete ul li": {
      borderRadius: "4px",
      padding: "2px 8px",
    },
    ".cm-tooltip-autocomplete ul li[aria-selected]": {
      backgroundColor: "color-mix(in srgb, var(--copper) 24%, transparent)",
      color: "var(--paper)",
    },
  });
}
