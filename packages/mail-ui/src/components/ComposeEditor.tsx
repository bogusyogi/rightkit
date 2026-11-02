import { forwardRef, useImperativeHandle, useRef, useState } from "react";
import { useEditor, EditorContent } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit"; // bundles Bold/Italic/Underline/Strike/Link/lists in v3
import { Image } from "@tiptap/extension-image";
import { TextStyle } from "@tiptap/extension-text-style";
import { Color } from "@tiptap/extension-color";
import { FontFamily } from "@tiptap/extension-font-family";
import { Extension } from "@tiptap/core";
import { IconButton, Input } from "@rightkit/react-shell/react";
import { sanitizeHtml } from "../lib/html.js";
import { showDialog } from "../hooks/dialog.js";

/** Minimal inline extension: stores fontSize as a textStyle attribute. */
const FontSize = Extension.create({
  name: "fontSize",
  addOptions: () => ({ types: ["textStyle"] }),
  addGlobalAttributes() {
    return [
      {
        types: this.options.types,
        attributes: {
          fontSize: {
            default: null,
            parseHTML: (el) => el.style.fontSize || null,
            renderHTML: (a) => (a.fontSize ? { style: `font-size:${a.fontSize}` } : {}),
          },
        },
      },
    ];
  },
  addCommands() {
    return {
      setFontSize:
        (size: string) =>
        ({ chain }: { chain: any }) =>
          chain().setMark("textStyle", { fontSize: size }).run(),
      unsetFontSize:
        () =>
        ({ chain }: { chain: any }) =>
          chain().setMark("textStyle", { fontSize: null }).run(),
    } as any;
  },
});

/** Imperative surface the compose flow drives the editor through. Mirrors the
 *  operations the old contentEditable supported (read/insert/replace/focus). */
export interface ComposeEditorHandle {
  getHTML(): string;
  getText(): string;
  setHTML(html: string): void;
  insertHTML(html: string): void;
  focus(): void;
}

type Props = { initialHtml: string };

/**
 * TipTap-based rich-text body for compose. Replaces the deprecated
 * `document.execCommand` contentEditable. Owns its formatting toolbar; the
 * parent reads/writes content only through the imperative handle. Pasted HTML
 * is DOMPurify-sanitized; pasted images are embedded inline as data URLs.
 */
export const ComposeEditor = forwardRef<ComposeEditorHandle, Props>(function ComposeEditor(
  { initialHtml },
  ref
) {
  const editor = useEditor({
    extensions: [
      StarterKit.configure({ link: { openOnClick: false, autolink: true } }),
      TextStyle,
      Color,
      FontFamily.configure({ types: ["textStyle"] }),
      FontSize,
      Image.configure({ inline: false }),
    ],
    content: initialHtml,
    editorProps: {
      attributes: {
        class: "compose-text",
        role: "textbox",
        "aria-multiline": "true",
        "aria-label": "Message body",
      },
      // Sanitize any pasted HTML before it enters the document.
      transformPastedHTML: sanitizeHtml,
      handlePaste: (_view, event) => {
        const items = Array.from(event.clipboardData?.items || []);
        const img = items.find((it) => it.kind === "file" && it.type.startsWith("image/"));
        if (!img) return false;
        const file = img.getAsFile();
        if (!file) return false;
        const reader = new FileReader();
        reader.onload = () => {
          const url = String(reader.result || "");
          if (url) editor?.chain().focus().setImage({ src: url }).run();
        };
        reader.readAsDataURL(file);
        return true; // we handled the image; stop default paste
      },
    },
  });

  useImperativeHandle(
    ref,
    (): ComposeEditorHandle => ({
      getHTML: () => editor?.getHTML() ?? "",
      getText: () => editor?.getText() ?? "",
      setHTML: (html) => editor?.commands.setContent(html),
      insertHTML: (html) => editor?.chain().focus().insertContent(html).run(),
      focus: () => editor?.chain().focus().run(),
    }),
    [editor]
  );

  // ARIA toolbar with roving tabindex: the bar is a SINGLE Tab stop (only the
  // active control is tabbable), ←/→ move between controls, and Tab moves on to
  // the editor body — so reaching the body from Subject is one Tab past the bar,
  // not a tab through every button.
  const toolbarRef = useRef<HTMLDivElement>(null);
  const colorRef = useRef<HTMLInputElement>(null);
  const [fmtActive, setFmtActive] = useState(0);

  // A toolbar button. `idx` is its position for roving tabindex. onMouseDown
  // keeps editor selection (no blur); Enter/Space activate it when focused.
  const btn = (idx: number, label: React.ReactNode, title: string, run: () => void | Promise<void> | boolean) => (
    <IconButton
      type="button"
      label={title}
      data-fmt-item
      tabIndex={idx === fmtActive ? 0 : -1}
      onMouseDown={async (e) => {
        e.preventDefault();
        await run();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          run();
        }
      }}
    >
      {label}
    </IconButton>
  );

  // ←/→/Home/End move focus among the toolbar's controls (roving tabindex).
  function onToolbarKey(e: React.KeyboardEvent) {
    if (!["ArrowRight", "ArrowLeft", "Home", "End"].includes(e.key)) return;
    const items = Array.from(
      toolbarRef.current?.querySelectorAll<HTMLElement>("[data-fmt-item]") ?? []
    );
    if (items.length === 0) return;
    e.preventDefault();
    let next: number;
    if (e.key === "ArrowRight") next = (fmtActive + 1) % items.length;
    else if (e.key === "ArrowLeft") next = (fmtActive - 1 + items.length) % items.length;
    else if (e.key === "Home") next = 0;
    else next = items.length - 1;
    setFmtActive(next);
    items[next]?.focus();
  }

  const chain = () => editor!.chain().focus();

  return (
    <>
      <div
        className="compose-format"
        role="toolbar"
        aria-label="Formatting"
        ref={toolbarRef}
        onKeyDown={onToolbarKey}
      >
        {editor && (
          <>
            {btn(0, <b>B</b>, "Bold", () => chain().toggleBold().run())}
            {btn(1, <i>I</i>, "Italic", () => chain().toggleItalic().run())}
            {btn(2, <u>U</u>, "Underline", () => chain().toggleUnderline().run())}
            {btn(3, <s>S</s>, "Strikethrough", () => chain().toggleStrike().run())}
            {btn(4, "A", "Text color", () => colorRef.current?.click())}
            <Input
              ref={colorRef}
              type="color"
              hidden
              tabIndex={-1}
              onChange={(e) => chain().setColor(e.target.value).run()}
            />
            <span className="fmt-sep" />
            {btn(5, "•", "Bulleted list", () => chain().toggleBulletList().run())}
            {btn(6, "1.", "Numbered list", () => chain().toggleOrderedList().run())}
            {btn(7, "Link", "Insert link", async () => {
              const url = await showDialog({ kind: "prompt", message: "Link URL (https://…)" });
              if (url && /^https?:\/\//i.test(url)) {
                chain().extendMarkRange("link").setLink({ href: url }).run();
              }
            })}
            <span className="fmt-sep" />
            {btn(8, "❝", "Blockquote", () => chain().toggleBlockquote().run())}
            <span className="fmt-sep" />
            <select
              className="fmt-select"
              data-fmt-item
              tabIndex={fmtActive === 9 ? 0 : -1}
              defaultValue=""
              onChange={(e) => {
                const v = e.target.value;
                if (v) chain().setHeading({ level: Number(v) as 1 | 2 | 3 }).run();
                else chain().setParagraph().run();
              }}
              title="Heading"
            >
              <option value="">¶</option>
              <option value="1">H1</option>
              <option value="2">H2</option>
              <option value="3">H3</option>
            </select>
            <select
              className="fmt-select"
              data-fmt-item
              tabIndex={fmtActive === 10 ? 0 : -1}
              defaultValue=""
              onChange={(e) => {
                if (e.target.value) chain().setFontFamily(e.target.value).run();
              }}
              title="Font family"
            >
              <option value="">Font</option>
              <option value="Arial">Arial</option>
              <option value="Georgia">Georgia</option>
              <option value="Times New Roman">Times New Roman</option>
              <option value="Courier New">Courier New</option>
              <option value="Verdana">Verdana</option>
            </select>
            <select
              className="fmt-select"
              data-fmt-item
              tabIndex={fmtActive === 11 ? 0 : -1}
              defaultValue=""
              onChange={(e) => {
                if (e.target.value) chain().setFontSize(e.target.value).run();
                else chain().unsetFontSize().run();
              }}
              title="Font size"
            >
              <option value="">Size</option>
              <option value="10px">10</option>
              <option value="12px">12</option>
              <option value="14px">14</option>
              <option value="16px">16</option>
              <option value="18px">18</option>
              <option value="24px">24</option>
              <option value="32px">32</option>
            </select>
            <span className="fmt-sep" />
            {btn(12, "Clear", "Clear formatting", () => chain().unsetAllMarks().clearNodes().run())}
          </>
        )}
      </div>
      <EditorContent editor={editor} />
    </>
  );
});
