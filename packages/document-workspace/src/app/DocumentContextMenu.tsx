// Right-click menu for the document content plane. The app globally suppresses the raw WebView2
// context menu (`blockNativeContextMenu`, installed in main.tsx) so the chrome never shows browser junk; this restores a curated,
// cross-platform Edit menu (Cut / Copy / Paste / Select all, plus Copy contents) on the actual document
// surfaces — markdown preview, source/CodeMirror, DOCX, HTML, PDF text, images. Scoped by the
// `[data-vr-content-plane]` marker so a right-click on the sidebar/toolbar still gets nothing.
import { useCallback, useEffect, useState } from "react";
import { ContextMenu, type MenuItem } from "@rightkit/react-shell/react";
import { compactMenuItemClass } from "./sidebar/SidebarPrimitives.js";

const EDITABLE_SELECTOR =
  '.cm-content, .ProseMirror, [contenteditable="true"], textarea, input:not([type="checkbox"]):not([type="radio"]):not([type="button"])';

export interface DocContextMenu {
  x: number;
  y: number;
  editable: boolean;
  hasSelection: boolean;
  editableEl: HTMLElement | null;
  contentRoot: HTMLElement | null;
}

export type DocMenuItemId = "cut" | "copy" | "paste" | "selectAll" | "copyContents";

export interface DocMenuItem {
  id: DocMenuItemId;
  label: string;
  disabled: boolean;
  separatorBefore?: boolean;
}

// Pure item composition, kept separate so the menu shape is unit-testable without a DOM or clipboard.
// Cut/Paste appear only on editable surfaces; Copy/Cut are disabled without a selection; Copy contents
// appears only when the caller can supply the whole-document text (text-like formats).
export function documentMenuItems(opts: {
  editable: boolean;
  hasSelection: boolean;
  canCopyContents: boolean;
}): DocMenuItem[] {
  const items: DocMenuItem[] = [];
  if (opts.editable) items.push({ id: "cut", label: "Cut", disabled: !opts.hasSelection });
  items.push({ id: "copy", label: "Copy", disabled: !opts.hasSelection });
  if (opts.editable) items.push({ id: "paste", label: "Paste", disabled: false });
  items.push({ id: "selectAll", label: "Select all", disabled: false, separatorBefore: true });
  if (opts.canCopyContents)
    items.push({ id: "copyContents", label: "Copy contents", disabled: false });
  return items;
}

// Non-HTML document surfaces use the host app curated menu on plain or Shift+right-click. HTML iframe
// events never reach this parent listener; those frames retain their own exact-node inspect menus.
export function useDocumentContextMenu() {
  const [menu, setMenu] = useState<DocContextMenu | null>(null);
  useEffect(() => {
    const onCtx = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      if (!target || typeof target.closest !== "function") return;
      const contentRoot = target.closest<HTMLElement>("[data-vr-content-plane]");
      if (!contentRoot) return; // chrome right-click → no document menu
      // Never shadow an HTML preview iframe. Right-clicks INSIDE the iframe are a separate document
      // and never reach this listener; this only guards the edge case where the iframe element itself
      // is the target. The HTML previews own their own menus — WebView2's native menu + the Rust-grafted
      // "View source"/Inspect on the interactive (about:srcdoc) frame, and HtmlViewer's inspect menu on
      // the safe (allow-same-origin) frame — and must stay authoritative there.
      if (target.closest("iframe")) return;
      e.preventDefault();
      const editableEl = target.closest<HTMLElement>(EDITABLE_SELECTOR);
      const editable = !!editableEl && !editableEl.closest('[contenteditable="false"]');
      const sel = typeof window.getSelection === "function" ? window.getSelection() : null;
      const hasSelection = !!sel && !sel.isCollapsed && sel.toString().trim().length > 0;
      setMenu({
        x: e.clientX,
        y: e.clientY,
        editable,
        hasSelection,
        editableEl: editableEl ?? null,
        contentRoot,
      });
    };
    document.addEventListener("contextmenu", onCtx, { capture: true });
    return () => document.removeEventListener("contextmenu", onCtx, { capture: true });
  }, []);
  return { menu, close: useCallback(() => setMenu(null), []) };
}

export function DocumentContextMenu({
  menu,
  onClose,
  onCopyContents,
}: {
  menu: DocContextMenu;
  onClose: () => void;
  onCopyContents: (() => void) | null;
}) {
  const items = documentMenuItems({
    editable: menu.editable,
    hasSelection: menu.hasSelection,
    canCopyContents: !!onCopyContents,
  });

  const run = (id: DocMenuItemId) => {
    switch (id) {
      case "cut":
        document.execCommand("cut");
        break;
      case "copy":
        document.execCommand("copy");
        break;
      case "paste":
        void (async () => {
          try {
            const text = await navigator.clipboard.readText();
            menu.editableEl?.focus();
            document.execCommand("insertText", false, text);
          } catch {
            /* clipboard unavailable / denied — nothing to paste */
          }
        })();
        break;
      case "selectAll":
        if (menu.editableEl) {
          menu.editableEl.focus();
          document.execCommand("selectAll");
        } else if (menu.contentRoot) {
          const sel = window.getSelection();
          sel?.removeAllRanges();
          const range = document.createRange();
          range.selectNodeContents(menu.contentRoot);
          sel?.addRange(range);
        }
        break;
      case "copyContents":
        onCopyContents?.();
        break;
    }
  };

  // The shell menu keeps the document selection (no focus steal, no mousedown default), handles
  // arrows/Home/End/Enter/Space/Escape from the window, closes on scrim press or context-menu
  // gesture, and fits itself to the viewport.
  const menuItems: MenuItem[] = items.map((item) => ({
    id: item.id,
    label: item.label,
    disabled: item.disabled,
    separatorBefore: item.separatorBefore,
    run: () => run(item.id),
  }));

  return (
    <ContextMenu
      x={menu.x}
      y={menu.y}
      items={menuItems}
      onClose={onClose}
      label="Document actions"
      width={176}
      itemAs="button"
      autoFocus={false}
      showLead={false}
      renderItem={(item) => item.label}
      className="vr-pop no-print overflow-x-hidden rounded-lg border border-line bg-panel-2 py-0.5 shadow-2xl"
      scrimClassName="no-print"
      slots={{
        item: `${compactMenuItemClass} [&.is-active]:bg-ink/50 [&.is-active]:text-paper [&.is-disabled]:cursor-default [&.is-disabled]:opacity-40 [&.is-disabled]:hover:bg-transparent`,
        separator: "my-1 h-px bg-line",
      }}
    />
  );
}
