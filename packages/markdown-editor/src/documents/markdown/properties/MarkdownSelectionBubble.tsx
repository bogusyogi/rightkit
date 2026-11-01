// Small formatting bubble that appears above a text selection in the live Markdown editor (the
// Bear/Notion pattern): the most common inline formats right where the user is working, without a
// trip to the toolbar. The full toolbar stays visible; this only shortens the path.
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { EditorView } from "prosemirror-view";
import { Code } from "@phosphor-icons/react/dist/icons/Code";
import { Highlighter } from "@phosphor-icons/react/dist/icons/Highlighter";
import { LinkSimple } from "@phosphor-icons/react/dist/icons/LinkSimple";
import { TextB } from "@phosphor-icons/react/dist/icons/TextB";
import { TextItalic } from "@phosphor-icons/react/dist/icons/TextItalic";
import { TextStrikethrough } from "@phosphor-icons/react/dist/icons/TextStrikethrough";
import { shortcutLabel } from "@rightkit/document-core/platform";

export type BubbleCommand =
  | "bold"
  | "italic"
  | "strikethrough"
  | "code"
  | "highlight"
  | "insert-link";

const ACTIONS: { id: BubbleCommand; label: string; Icon: typeof TextB }[] = [
  { id: "bold", label: `Bold (${shortcutLabel("mod+b")})`, Icon: TextB },
  { id: "italic", label: `Italic (${shortcutLabel("mod+i")})`, Icon: TextItalic },
  { id: "strikethrough", label: "Strikethrough", Icon: TextStrikethrough },
  { id: "code", label: "Inline code", Icon: Code },
  { id: "highlight", label: "Highlight", Icon: Highlighter },
  { id: "insert-link", label: "Link", Icon: LinkSimple },
];

export function MarkdownSelectionBubble({
  getEditorView,
  onCommand,
}: {
  getEditorView?: () => EditorView | null;
  onCommand: (id: BubbleCommand) => void;
}) {
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useEffect(() => {
    let frame = 0;
    const update = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const view = getEditorView?.();
        const sel = view?.state.selection;
        if (!view || !sel || sel.empty || !view.hasFocus() || !view.editable) return setPos(null);
        const start = view.coordsAtPos(sel.from);
        const end = view.coordsAtPos(sel.to);
        const left = (Math.min(start.left, end.left) + Math.max(start.right, end.right)) / 2;
        const top = Math.min(start.top, end.top);
        // Hide rather than float detached when the selection's top has scrolled out of view.
        if (top < 60) return setPos(null);
        setPos({ left, top });
      });
    };
    document.addEventListener("selectionchange", update);
    // Some engines finish a mouse selection without a final selectionchange; cover the release.
    document.addEventListener("mouseup", update);
    document.addEventListener("keyup", update);
    window.addEventListener("scroll", update, true);
    window.addEventListener("resize", update);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("selectionchange", update);
      document.removeEventListener("mouseup", update);
      document.removeEventListener("keyup", update);
      window.removeEventListener("scroll", update, true);
      window.removeEventListener("resize", update);
    };
  }, [getEditorView]);

  if (!pos) return null;
  return createPortal(
    <div
      role="toolbar"
      aria-label="Format selection"
      data-vr-selection-bubble=""
      className="no-print vr-capsule vr-over-content fixed z-[60] h-9 -translate-x-1/2 -translate-y-full gap-0.5 px-1"
      style={{ left: pos.left, top: pos.top - 8 }}
    >
      {ACTIONS.map(({ id, label, Icon }) => (
        <button
          key={id}
          type="button"
          aria-label={label}
          title={label}
          // Keep the editor's selection: act on mousedown and never take focus.
          onMouseDown={(event) => {
            event.preventDefault();
            onCommand(id);
          }}
          className="vr-focus grid h-7 w-7 place-items-center rounded-full text-paper/85 hover:bg-ink/30 hover:text-paper"
        >
          <Icon size={15} weight="regular" aria-hidden />
        </button>
      ))}
    </div>,
    document.body,
  );
}
