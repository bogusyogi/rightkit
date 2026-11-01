import type { Node as ProseMirrorNode } from "prosemirror-model";
import type { EditorView, NodeView } from "prosemirror-view";
import { NodeSelection } from "prosemirror-state";
import * as React from "react";
import { createRoot } from "react-dom/client";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import { imageNodeToMarkdown } from "../projection/pmToMarkdown";

export interface MarkdownImageNodeViewContext {
  core: MarkdownDocumentCore | null;
  documentPath?: string;
}

/**
 * ProseMirror NodeView for the inline `image` atom.
 *
 * Renders a brokered `<img>` preview plus contextual edit controls:
 * - Click/Enter opens alt/title edit
 * - Backspace/Delete removes via normal PM transaction (no custom handling needed)
 * - Escape restores selection without mutation
 * - Save commits through the core with source-range patching and revision safety
 */
export class MarkdownImageNodeView implements NodeView {
  dom: HTMLElement;
  contentDOM?: HTMLElement | null;
  private node: ProseMirrorNode;
  private view: EditorView;
  private getPos: () => number | undefined;
  private ctx: MarkdownImageNodeViewContext;
  private root: ReturnType<typeof createRoot> | null = null;
  private container: HTMLSpanElement | null = null;

  constructor(
    node: ProseMirrorNode,
    view: EditorView,
    getPos: () => number | undefined,
    ctx: MarkdownImageNodeViewContext,
  ) {
    this.node = node;
    this.view = view;
    this.getPos = getPos;
    this.ctx = ctx;

    this.dom = document.createElement("span");
    this.dom.className = "vr-image-node inline-flex align-middle max-w-full relative group";
    this.dom.setAttribute("data-vr-image", "1");
    this.dom.contentEditable = "false";

    this.container = document.createElement("span");
    this.container.className = "vr-image-inner inline-flex items-center";
    this.dom.appendChild(this.container);

    this.contentDOM = null;
    this.root = createRoot(this.container);
    this.render();

    // Click to select atom; double-click or Enter opens editor
    this.dom.addEventListener("click", (e) => {
      e.preventDefault();
      const pos = this.getPos();
      if (typeof pos === "number") {
        const tr = this.view.state.tr.setSelection(NodeSelection.create(this.view.state.doc, pos));
        this.view.dispatch(tr);
        this.view.focus();
      }
    });
    this.dom.addEventListener("dblclick", (e) => {
      e.preventDefault();
      this.openEditor();
    });
    this.dom.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        this.openEditor();
      }
      if (e.key === "Escape") {
        e.preventDefault();
        this.closeEditor();
      }
    });
    this.dom.tabIndex = 0;
  }

  update(node: ProseMirrorNode): boolean {
    if (node.type.name !== "image") return false;
    if (
      node.attrs.src !== this.node.attrs.src ||
      node.attrs.alt !== this.node.attrs.alt ||
      node.attrs.title !== this.node.attrs.title
    ) {
      this.node = node;
      this.render();
    } else {
      this.node = node;
    }
    return true;
  }

  selectNode() {
    this.dom.classList.add("vr-image-selected", "ring-2", "ring-copper/50");
  }

  deselectNode() {
    this.dom.classList.remove("vr-image-selected", "ring-2", "ring-copper/50");
    this.closeEditor(false);
  }

  stopEvent(event: Event): boolean {
    // Allow events inside the editor popover to be handled by React
    const target = event.target as HTMLElement | null;
    if (target?.closest?.("[data-vr-image-editor]")) return true;
    return false;
  }

  ignoreMutation(): boolean {
    return true;
  }

  destroy() {
    const root = this.root;
    this.root = null;
    if (root) queueMicrotask(() => root.unmount());
  }

  private editing = false;

  private openEditor() {
    if (this.editing) return;
    this.editing = true;
    this.render();
  }

  private closeEditor(restoreFocus = true) {
    if (!this.editing) return;
    this.editing = false;
    this.render();
    if (restoreFocus) {
      try {
        this.view.focus();
      } catch {
        // ignore
      }
    }
  }

  private render() {
    if (!this.root || !this.container) return;
    const { src, alt, title } = this.node.attrs as {
      src: string;
      alt: string;
      title: string | null;
    };
    const editing = this.editing;
    this.root.render(
      React.createElement(ImageInlineView, {
        src,
        alt,
        title,
        editing,
        onStartEdit: () => this.openEditor(),
        onCancel: () => this.closeEditor(true),
        onSave: (nextAlt: string, nextTitle: string | null, nextSrc: string) => {
          const error = this.commitEdit(nextAlt, nextTitle, nextSrc);
          if (!error) this.closeEditor(true);
          return error;
        },
        documentPath: this.ctx.documentPath,
      }),
    );
  }

  private commitEdit(nextAlt: string, nextTitle: string | null, nextSrc: string): string | null {
    const pos = this.getPos();
    if (typeof pos !== "number") return "Image is no longer available.";
    if (!isAllowedImageSource(nextSrc))
      return "Image source must be a relative path or http(s) URL.";
    if (!this.ctx.core) {
      // Fallback: mutate node directly via PM transaction when no core
      const tr = this.view.state.tr.setNodeMarkup(pos, undefined, {
        ...this.node.attrs,
        src: nextSrc,
        alt: nextAlt,
        title: nextTitle,
      });
      this.view.dispatch(tr);
      return null;
    }
    // Use core source-range patching when possible for byte-identical preservation
    const core = this.ctx.core;
    const snapshot = core.snapshot();
    const source = snapshot.logicalSource;
    const sourceFrom = Number(this.node.attrs.sourceFrom ?? -1);
    const sourceTo = Number(this.node.attrs.sourceTo ?? -1);
    const from = sourceFrom;
    const to = sourceTo;
    // Validate source range; if invalid or node was created in PM (no source), fall back to PM markup edit
    if (from < 0 || to < from || to > source.length) {
      const tr = this.view.state.tr.setNodeMarkup(pos, undefined, {
        ...this.node.attrs,
        src: nextSrc,
        alt: nextAlt,
        title: nextTitle,
        rawSource: imageNodeToMarkdown(this.view.state.doc.nodeAt(pos) ?? this.node),
      });
      this.view.dispatch(tr);
      // Also attempt to sync to core via PM->source patch path (commitPmTransactionToCore will handle fallback)
      return null;
    }
    // Verify rawSource matches current logical slice (defensive against stale mapping)
    const raw = this.node.attrs.rawSource as string | undefined;
    if (raw && source.slice(from, to) !== raw) return "Image changed in Source; reopen its editor.";
    const newMarkdown = imageNodeToMarkdown(
      this.node.type.create({
        ...this.node.attrs,
        src: nextSrc,
        alt: nextAlt,
        title: nextTitle,
        rawSource: "",
      }),
    );
    try {
      core.applyCommand({
        edits: [{ from, to, insert: newMarkdown, inheritMode: "preserve" as const }],
        expectedRevision: snapshot.revision,
        origin: "hybrid",
        label: "Format",
        isolateHistory: true,
      });
    } catch {
      // A stale source revision is resolved by the core subscription. Do not
      // dispatch a PM fallback that could overwrite newer source text.
      return "Image changed in Source; reopen its editor.";
    }
    return null;
  }
}

function isAllowedImageSource(source: string): boolean {
  const value = source.trim();
  if (!value || value.startsWith("//")) return false;
  if (/^https?:\/\//i.test(value)) return true;
  return !/^[a-z][a-z0-9+.-]*:/i.test(value);
}

function ImageInlineView(props: {
  src: string;
  alt: string;
  title: string | null;
  editing: boolean;
  onStartEdit: () => void;
  onCancel: () => void;
  onSave: (alt: string, title: string | null, src: string) => string | null;
  documentPath?: string;
}) {
  const { src, alt, title, editing, onStartEdit, onCancel, onSave } = props;
  const [editAlt, setEditAlt] = React.useState(alt);
  const [editTitle, setEditTitle] = React.useState(title ?? "");
  const [editSrc, setEditSrc] = React.useState(src);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => {
    setEditAlt(alt);
    setEditTitle(title ?? "");
    setEditSrc(src);
    setError(null);
  }, [alt, title, src]);

  const [previewSrc, setPreviewSrc] = React.useState(src);
  React.useEffect(() => {
    let cancelled = false;
    setPreviewSrc(src);
    if (!src || /^https?:\/\//i.test(src) || src.startsWith("data:")) {
      return;
    }
    // Local relative path -> try brokered read
    const documentPath = props.documentPath;
    if (documentPath) {
      import("@rightkit/document-core/state/tauriBridge").then(({ readRelativeAsset }) => {
        void readRelativeAsset(documentPath, src)
          .then((asset) => {
            if (!cancelled && asset) setPreviewSrc(`data:${asset.mime};base64,${asset.base64}`);
          })
          .catch(() => {});
      });
    }
    return () => {
      cancelled = true;
    };
  }, [src, props.documentPath]);

  if (!editing) {
    return React.createElement(
      "span",
      { className: "vr-image-preview inline-flex items-center gap-1" },
      React.createElement("img", {
        src: previewSrc,
        alt,
        title: title ?? undefined,
        className: "max-h-48 max-w-full rounded border border-line bg-panel object-contain",
        draggable: false,
        onError: (e) => {
          // Keep alt text visible on failure
          (e.target as HTMLImageElement).style.display = "none";
        },
      }),
      React.createElement(
        "button",
        {
          type: "button",
          className:
            "vr-image-edit-trigger ml-1 rounded border border-line bg-panel-2 px-1.5 py-0.5 text-xs text-muted hover:text-paper",
          onClick: onStartEdit,
          title: "Edit image (Alt, Title, URL)",
        },
        "Edit",
      ),
    );
  }

  return React.createElement(
    "span",
    {
      "data-vr-image-editor": "1",
      className:
        "vr-image-editor absolute left-0 top-full z-10 mt-2 flex w-[320px] flex-col gap-2 rounded-md border border-line bg-panel p-3 shadow-xl",
      onKeyDown: (e: React.KeyboardEvent) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onCancel();
        }
      },
    },
    React.createElement(
      "label",
      { className: "flex flex-col gap-1 text-xs" },
      React.createElement("span", { className: "font-semibold text-paper" }, "Alt text"),
      React.createElement(
        "span",
        { className: "text-chrome-xs text-muted-2" },
        "Description for screen readers & failed loading. Leave empty for decorative.",
      ),
      React.createElement("input", {
        type: "text",
        value: editAlt,
        onChange: (e: React.ChangeEvent<HTMLInputElement>) => setEditAlt(e.target.value),
        placeholder: "Describe the image",
        className: "rounded border border-line bg-ink px-2 py-1 text-xs text-paper",
        autoFocus: true,
      }),
    ),
    error ? React.createElement("span", { className: "text-chrome-xs text-red-300" }, error) : null,
    React.createElement(
      "label",
      { className: "flex flex-col gap-1 text-xs" },
      React.createElement("span", { className: "font-semibold text-paper" }, "Title (optional)"),
      React.createElement("input", {
        type: "text",
        value: editTitle,
        onChange: (e: React.ChangeEvent<HTMLInputElement>) => setEditTitle(e.target.value),
        placeholder: "Optional title shown on hover",
        className: "rounded border border-line bg-ink px-2 py-1 text-xs text-paper",
      }),
    ),
    React.createElement(
      "label",
      { className: "flex flex-col gap-1 text-xs" },
      React.createElement("span", { className: "font-semibold text-paper" }, "Source"),
      React.createElement("input", {
        type: "text",
        value: editSrc,
        onChange: (e: React.ChangeEvent<HTMLInputElement>) => setEditSrc(e.target.value),
        placeholder: "https://example.com/image.png or ./assets/image.png",
        className: "rounded border border-line bg-ink px-2 py-1 text-xs text-paper",
      }),
    ),
    React.createElement(
      "span",
      { className: "flex gap-2" },
      React.createElement(
        "button",
        {
          type: "button",
          className:
            "flex-1 rounded bg-copper px-3 py-1.5 text-xs font-semibold text-white hover:bg-copper/90",
          onClick: () => setError(onSave(editAlt, editTitle || null, editSrc)),
        },
        "Apply",
      ),
      React.createElement(
        "button",
        {
          type: "button",
          className:
            "rounded border border-line bg-panel-2 px-3 py-1.5 text-xs text-muted hover:text-paper",
          onClick: onCancel,
        },
        "Cancel",
      ),
    ),
  );
}
