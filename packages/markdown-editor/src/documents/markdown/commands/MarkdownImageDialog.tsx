import * as React from "react";
import type { EditorView } from "prosemirror-view";
import { ModalShell } from "@rightkit/react-shell/react";
import { DIALOG_SLOTS } from "@rightkit/document-core/app/chrome";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import {
  altPrefillFromFileName,
  insertImageUrl,
  insertLocalMarkdownImage,
  isHttpUrl,
  isImageMime,
  kindForMime,
  mimeFromFileName,
} from "../assets/markdownImage";
import { MAX_MARKDOWN_ASSET_BYTES, validateMarkdownAsset } from "../assets/markdownAsset";

export interface MarkdownImageDialogProps {
  readonly core: MarkdownDocumentCore;
  readonly view?: EditorView | null;
  readonly documentPath?: string;
  readonly onClose: () => void;
  readonly onSaveAsSuccess?: (newPath: string) => void;
  readonly getDocumentText?: () => string;
  readonly initialSelection?: { from: number; to: number };
}

export function MarkdownImageDialog(props: MarkdownImageDialogProps): React.ReactElement {
  const { core, view, documentPath, onClose, onSaveAsSuccess, getDocumentText, initialSelection } =
    props;
  const [alt, setAlt] = React.useState("");
  const [title, setTitle] = React.useState("");
  const [url, setUrl] = React.useState("");
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const fileInputRef = React.useRef<HTMLInputElement>(null);
  const [chosenFile, setChosenFile] = React.useState<{
    bytes: Uint8Array;
    name: string;
    mime: string;
  } | null>(null);
  const [chosenKind, setChosenKind] = React.useState<
    import("../assets/markdownAsset").MarkdownAssetMimeKind | null
  >(null);
  const dialogRef = React.useRef<HTMLDivElement>(null);
  const restoreFocusRef = React.useRef<HTMLElement | null>(null);
  const viewRef = React.useRef(view);
  viewRef.current = view;

  React.useEffect(() => {
    restoreFocusRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const firstInput = dialogRef.current?.querySelector<HTMLInputElement>(
      'input:not([type="hidden"]):not([type="file"])',
    );
    firstInput?.focus();
    return () => {
      const restore = restoreFocusRef.current;
      // The opener (slash menu, toolbar popover) may be unmounted by now; a detached element
      // would leave focus on <body> and send Cmd+Z to native contenteditable undo.
      if (restore?.isConnected) restore.focus();
      else viewRef.current?.focus();
    };
  }, []);

  const handleChooseFile = async (file: File) => {
    const mimeGuess = file.type || mimeFromFileName(file.name) || "";
    const kind =
      kindForMime(mimeGuess) ??
      (mimeFromFileName(file.name) as
        | import("../assets/markdownAsset").MarkdownAssetMimeKind
        | null);
    if (!kind) {
      setError(`Unsupported image type: ${file.type || file.name.split(".").pop()}`);
      return;
    }
    if (!isImageMime(mimeGuess) && !kind) {
      setError("Please choose a PNG, JPEG, WebP, GIF, or SVG image.");
      return;
    }
    let buf: Uint8Array;
    try {
      buf = new Uint8Array(await file.arrayBuffer());
    } catch {
      setError("Could not read image file.");
      return;
    }
    const validationError = validateMarkdownAsset({
      source: buf,
      hintName: file.name,
      mimeKind: kind,
    });
    if (validationError) {
      setError(
        validationError.code === "size-limit"
          ? `Image exceeds ${MAX_MARKDOWN_ASSET_BYTES / (1024 * 1024)} MiB. Choose a smaller file.`
          : validationError.message,
      );
      return;
    }
    setChosenFile({ bytes: buf, name: file.name, mime: mimeGuess || `image/${kind}` });
    setChosenKind(kind);
    if (!alt) setAlt(altPrefillFromFileName(file.name));
    setError(null);
  };

  const onFileInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const f = e.target.files?.[0];
    if (f) void handleChooseFile(f);
    // reset so same file can be re-chosen
    if (fileInputRef.current) fileInputRef.current.value = "";
  };

  const onPaste = async (e: React.ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (!item) continue;
      if (item.kind === "file" && item.type.startsWith("image/")) {
        e.preventDefault();
        const file = item.getAsFile();
        if (file) await handleChooseFile(file);
        return;
      }
    }
    // Also check for files via DataTransfer files (copied-file)
    const files = e.clipboardData?.files;
    if (files && files.length > 0) {
      const f = files[0];
      if (!f) return;
      if (isImageMime(f.type) || mimeFromFileName(f.name)) {
        e.preventDefault();
        await handleChooseFile(f);
        return;
      }
    }
  };

  const onDropFiles = async (e: React.DragEvent) => {
    e.preventDefault();
    const files = Array.from(e.dataTransfer?.files ?? []);
    const images = files.filter((f) => isImageMime(f.type) || mimeFromFileName(f.name));
    if (!images.length) return;
    // For dialog drop we only take first; canvas drop handles multiples
    const firstImage = images[0];
    if (firstImage) await handleChooseFile(firstImage);
  };

  const doInsertLocal = async () => {
    if (!chosenFile || !chosenKind) {
      setError("Choose an image first.");
      return;
    }
    setBusy(true);
    setError(null);
    const result = await insertLocalMarkdownImage({
      core,
      view: view ?? null,
      documentPath,
      file: { bytes: chosenFile.bytes, name: chosenFile.name, mime: chosenKind },
      hintName: chosenFile.name,
      mimeKind: chosenKind,
      alt,
      title: title || null,
      selection: initialSelection,
      getDocumentText,
      onSaveAsSuccess,
    });
    setBusy(false);
    if (!result.ok) {
      setError(result.error ?? "Insert failed");
      return;
    }
    onClose();
  };

  const doInsertUrl = async () => {
    const trimmed = url.trim();
    if (!isHttpUrl(trimmed)) {
      setError("URL must be http:// or https://");
      return;
    }
    setBusy(true);
    const res = await insertImageUrl({
      core,
      view: view ?? null,
      documentPath,
      selection: initialSelection,
      url: trimmed,
      alt,
      title: title || null,
    });
    setBusy(false);
    if (!res.ok) {
      setError(res.error ?? "Insert failed");
      return;
    }
    onClose();
  };

  const body = React.createElement(
      "div",
      {
        ref: dialogRef,
        onPaste,
        onDragOver: (e: React.DragEvent) => e.preventDefault(),
        onDrop: onDropFiles,
      },
      React.createElement(
        "h2",
        { id: "markdown-image-dialog-title", className: "mb-3 text-sm font-semibold text-paper" },
        "Insert image",
      ),
      React.createElement(
        "div",
        { className: "flex flex-col gap-3" },
        // Choose image
        React.createElement(
          "div",
          { className: "flex flex-col gap-1" },
          React.createElement(
            "span",
            { className: "text-xs font-semibold text-paper" },
            "Local image",
          ),
          React.createElement(
            "div",
            { className: "flex gap-2" },
            React.createElement(
              "button",
              {
                type: "button",
                className:
                  "rounded border border-line bg-panel-2 px-3 py-1.5 text-xs text-paper hover:bg-ink",
                onClick: () => fileInputRef.current?.click(),
                disabled: busy,
              },
              "Choose image…",
            ),
            chosenFile
              ? React.createElement(
                  "span",
                  { className: "truncate text-xs text-muted" },
                  chosenFile.name,
                )
              : React.createElement(
                  "span",
                  { className: "text-xs text-muted-2" },
                  "PNG, JPEG, WebP, GIF, SVG ≤20 MiB",
                ),
          ),
          React.createElement("input", {
            ref: fileInputRef,
            type: "file",
            accept: "image/png,image/jpeg,image/webp,image/gif,image/svg+xml",
            className: "hidden",
            onChange: onFileInputChange,
          }),
          React.createElement(
            "span",
            { className: "text-chrome-xs text-muted-2" },
            "Drop, paste screenshot, or copy-paste a file here.",
          ),
        ),
        // Alt
        React.createElement(
          "label",
          { className: "flex flex-col gap-1" },
          React.createElement(
            "span",
            { className: "text-xs font-semibold text-paper" },
            "Alt text",
          ),
          React.createElement(
            "span",
            { className: "text-chrome-xs text-muted-2" },
            "Description for screen readers & failed loading. Empty = decorative.",
          ),
          React.createElement("input", {
            type: "text",
            value: alt,
            onChange: (e: React.ChangeEvent<HTMLInputElement>) => setAlt(e.target.value),
            placeholder: "e.g. Checkout flow diagram",
            className: "rounded border border-line bg-ink px-2 py-1.5 text-xs text-paper",
          }),
        ),
        // Title
        React.createElement(
          "label",
          { className: "flex flex-col gap-1" },
          React.createElement(
            "span",
            { className: "text-xs font-semibold text-paper" },
            "Title (optional)",
          ),
          React.createElement("input", {
            type: "text",
            value: title,
            onChange: (e: React.ChangeEvent<HTMLInputElement>) => setTitle(e.target.value),
            placeholder: "Shown on hover",
            className: "rounded border border-line bg-ink px-2 py-1.5 text-xs text-paper",
          }),
        ),
        // Or URL
        React.createElement("div", { className: "my-1 border-t border-line" }),
        React.createElement(
          "label",
          { className: "flex flex-col gap-1" },
          React.createElement(
            "span",
            { className: "text-xs font-semibold text-paper" },
            "Or image URL (http/https)",
          ),
          React.createElement("input", {
            type: "text",
            value: url,
            onChange: (e: React.ChangeEvent<HTMLInputElement>) => setUrl(e.target.value),
            placeholder: "https://example.com/image.png",
            className: "rounded border border-line bg-ink px-2 py-1.5 text-xs text-paper",
          }),
        ),
        error
          ? React.createElement(
              "div",
              {
                className:
                  "rounded border border-red-500/30 bg-red-500/10 px-2 py-1 text-xs text-red-300",
              },
              error,
            )
          : null,
        React.createElement(
          "div",
          { className: "flex gap-2 pt-1" },
          React.createElement(
            "button",
            {
              type: "button",
              className:
                "flex-1 rounded bg-copper px-3 py-1.5 text-xs font-semibold text-white hover:bg-copper/90 disabled:opacity-50",
              onClick: () => {
                if (chosenFile) void doInsertLocal();
                else if (url.trim()) void doInsertUrl();
                else setError("Choose an image or enter a URL.");
              },
              disabled: busy,
            },
            busy ? "Inserting…" : "Insert",
          ),
          React.createElement(
            "button",
            {
              type: "button",
              className:
                "rounded border border-line bg-panel-2 px-3 py-1.5 text-xs text-muted hover:text-paper",
              onClick: onClose,
              disabled: busy,
            },
            "Cancel",
          ),
        ),
      ),
    );

  // The modal shell owns Escape and backdrop dismissal; while an insert is running it refuses both.
  return (
    <ModalShell
      label="Insert image"
      labelledBy="markdown-image-dialog-title"
      onClose={onClose}
      width={420}
      autoFocus={false}
      dismissible={!busy}
      slots={{ overlay: DIALOG_SLOTS.overlay, root: DIALOG_SLOTS.root }}
    >
      {body}
    </ModalShell>
  );
}
