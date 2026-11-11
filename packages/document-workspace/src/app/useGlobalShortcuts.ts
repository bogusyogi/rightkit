import { useEffect, useRef } from "react";
import { createShortcutRegistry } from "@rightkit/react-shell";
import type { ShortcutDef } from "@rightkit/react-shell";
import { capabilitiesFor } from "../documents/capabilities.js";
import type { LoadedDocument, SearchProvider } from "@rightkit/document-core/documents/types";
import type { DocxMenuApi } from "@rightkit/docx-editor/documents/docx/menuApi";
import { documentEvent } from "@rightkit/document-core/host";

interface Options {
  active: LoadedDocument | null;
  activeEditable: boolean;
  searchProvider: SearchProvider | null;
  canOpenFind?: boolean;
  docxMenu: DocxMenuApi | null;
  newDocument: () => void;
  onOpenDialog: () => void;
  onPrint: (doc: LoadedDocument | null) => void;
  onSave: (doc: LoadedDocument) => void;
  onSaveAs: (doc: LoadedDocument) => void;
  requestClose: (path: string) => void;
  togglePalette: () => void;
  openFind: (replace: boolean) => void;
  /** HTML Ctrl+S: one dirty page saves directly; several open the page-aware chooser. */
  onHtmlSaveShortcut: (entryPath: string) => void;
  /** Re-read the active doc from disk (browser-style refresh) — for non-HTML formats. HTML refresh is
   *  routed through a window event so the HtmlDocument reloads its current page + rebuilds the preview. */
  reloadActiveFromDisk: (doc: LoadedDocument) => void;
  /** Open whatever's on the clipboard (Ctrl+V / Ctrl+Shift+V): text → new draft, image → temp .bmp,
   *  files → open like drag-drop. Only fires outside a text field so editors keep their own paste. */
  openFromClipboard: () => void;
  /** Tab navigation: cycle next (+1) / previous (-1) through the open tabs. */
  cycleTab: (dir: 1 | -1) => void;
  /** Jump to the Nth open tab (0-based; the help overlay maps Ctrl+1..9). */
  jumpToTab: (index: number) => void;
  /** Reopen the most-recently-closed file/web tab (Ctrl+Shift+T). */
  reopenClosed: () => void;
  /** Reset the active surface's zoom/font size to its default (Ctrl+0). */
  resetZoom: () => void;
  /** Toggle source/text word wrap (Alt+Z). */
  toggleWordWrap: () => void;
  /** Toggle the sidebar (Ctrl+E / Ctrl+\). */
  toggleSidebar: () => void;
  /** Toggle the tab-overview card grid (Ctrl/Cmd+Shift+A). */
  toggleTabOverview: () => void;
  /** Show the keyboard-shortcut help overlay (?). */
  showHelp: () => void;
  /** Whether the find bar is currently open (drives F3 step-vs-open). */
  findOpen: boolean;
  /** Toggle distraction-free reading (Ctrl+Shift+F) — hides sidebar + toolbars. */
  toggleDistractionFree: () => void;
}

// Global keyboard map, registered on the RightKit shortcut registry. Modifier shortcuts: Ctrl/Cmd+N new,
// +O open, +Shift+P palette; +F find, +H replace; +W close; +P print; +S save, +Shift+S save as; +Tab /
// +Shift+Tab cycle tabs; +1..9 jump to tab; +Shift+T reopen closed; +0 reset zoom; +E toggle sidebar. Bare
// keys (guarded against focused editors): Alt+Z word wrap, ? help overlay. DOCX routes save/print through
// its bridge.
export function useGlobalShortcuts({
  active,
  activeEditable,
  searchProvider,
  canOpenFind,
  docxMenu,
  newDocument,
  onOpenDialog,
  onPrint,
  onSave,
  onSaveAs,
  requestClose,
  togglePalette,
  openFind,
  onHtmlSaveShortcut,
  reloadActiveFromDisk,
  openFromClipboard,
  cycleTab,
  jumpToTab,
  reopenClosed,
  resetZoom,
  toggleWordWrap,
  toggleSidebar,
  toggleTabOverview,
  showHelp,
  findOpen,
  toggleDistractionFree,
}: Options) {
  const latest = {
    active,
    activeEditable,
    searchProvider,
    canOpenFind,
    docxMenu,
    newDocument,
    onOpenDialog,
    onPrint,
    onSave,
    onSaveAs,
    requestClose,
    openFind,
    onHtmlSaveShortcut,
    reloadActiveFromDisk,
    openFromClipboard,
    cycleTab,
    jumpToTab,
    reopenClosed,
    resetZoom,
    toggleWordWrap,
    toggleSidebar,
    toggleTabOverview,
    showHelp,
    findOpen,
    togglePalette,
    toggleDistractionFree,
  };
  const latestRef = useRef(latest);
  latestRef.current = latest;

  useEffect(() => {
    const registry = createShortcutRegistry();
    // The former window listener ran for every keydown, even one another handler had already prevented
    // (editors and previews call preventDefault on their own keys) or one during IME composition. Each
    // binding keeps that behaviour, so the shell chords still fire exactly as before.
    const bind = (def: ShortcutDef) =>
      registry.register({ allowDefaultPrevented: true, allowComposing: true, ...def });
    // Gated bindings act on the active document; the rest work with no document open.
    const hasActive = () => latestRef.current.active !== null;

    // Browser-style refresh: HTML re-reads its current page + rebuilds the preview (via the window
    // event HtmlDocument listens for); other disk docs re-read from disk. Always preventDefault on the
    // caller so F5/Ctrl+R never reload the whole WebView2 app (which would blow away all open tabs).
    const reloadActiveDoc = () => {
      const { active, reloadActiveFromDisk } = latestRef.current;
      if (!active) return;
      if (active.format === "html") window.dispatchEvent(new Event(documentEvent("html-reload")));
      else if (active.format === "web") window.dispatchEvent(new Event(documentEvent("web-reload")));
      else reloadActiveFromDisk(active);
    };
    bind({
      id: "reload",
      label: "Reload",
      chord: ["f5", "ctrlormeta+r", "ctrlormeta+shift+r"],
      allowInInput: true,
      run: () => {
        reloadActiveDoc();
      },
    });

    // Browser-native inspect accelerators are consumed so WebView2/WKWebView never opens the whole-app
    // inspector. HTML preview frames own these shortcuts & forward them through their scoped bridge;
    // events originating inside an iframe do not bubble into this document. Cmd/Ctrl+Opt+I is the
    // reliable Mac path because bare F12 is normally volume-up; Ctrl+Shift+I is the Windows/Linux path.
    bind({
      id: "inspector-consume",
      label: "Inspector (consumed)",
      hidden: true,
      chord: ["f12", "ctrlormeta+alt+i", "ctrlormeta+shift+i"],
      allowInInput: true,
      run: () => undefined,
    });

    // F3 / Shift+F3 step find next/previous (works even inside an editor — it's not a character).
    // If the bar is open, let it own the step; otherwise open it.
    const findStep = (dir: 1 | -1) => {
      const { findOpen, canOpenFind, searchProvider, openFind } = latestRef.current;
      if (findOpen) window.dispatchEvent(new CustomEvent(documentEvent("find-step"), { detail: { dir } }));
      else if (canOpenFind ?? !!searchProvider) openFind(false);
    };
    bind({
      id: "find-next",
      label: "Find next",
      chord: "f3",
      allowInInput: true,
      run: () => {
        findStep(1);
      },
    });
    bind({
      id: "find-previous",
      label: "Find previous",
      chord: "shift+f3",
      allowInInput: true,
      run: () => {
        findStep(-1);
      },
    });

    // Bare keys: the registry drops these while focus is in a text field (allowInInput: false).
    bind({
      id: "word-wrap",
      label: "Toggle word wrap",
      chord: ["alt+z", "alt+shift+z"],
      allowInInput: false,
      run: () => {
        latestRef.current.toggleWordWrap();
      },
    });
    bind({
      id: "help",
      label: "Keyboard shortcuts",
      chord: "?",
      allowInInput: false,
      run: () => {
        latestRef.current.showHelp();
      },
    });

    // Ctrl/Cmd+V (and the explicit Ctrl/Cmd+Shift+V) → open whatever's on the clipboard. Suppressed inside
    // a text field so an editor's own paste keeps working there.
    bind({
      id: "paste",
      label: "Open clipboard",
      chord: ["ctrlormeta+v", "ctrlormeta+shift+v"],
      allowInInput: false,
      run: () => {
        latestRef.current.openFromClipboard();
      },
    });

    // Tab navigation (independent of the active doc).
    bind({
      id: "tab-next",
      label: "Next tab",
      chord: "ctrlormeta+tab",
      run: () => {
        latestRef.current.cycleTab(1);
      },
    });
    bind({
      id: "tab-previous",
      label: "Previous tab",
      chord: "ctrlormeta+shift+tab",
      run: () => {
        latestRef.current.cycleTab(-1);
      },
    });
    bind({
      id: "tab-jump",
      label: "Jump to tab",
      chord: Array.from({ length: 9 }, (_, i) => `ctrlormeta+${i + 1}`),
      run: (event) => {
        latestRef.current.jumpToTab(Number(event.key) - 1); // 9 is mapped to "last tab" by the handler
      },
    });
    bind({
      id: "new-document-tab",
      label: "New document",
      chord: "ctrlormeta+t", // Ctrl/Cmd+T — new untitled tab, same as the strip's "+"
      run: () => {
        latestRef.current.newDocument();
      },
    });
    bind({
      id: "reopen-closed",
      label: "Reopen closed tab",
      chord: "ctrlormeta+shift+t",
      run: () => {
        latestRef.current.reopenClosed();
      },
    });
    bind({
      id: "toggle-sidebar",
      label: "Toggle sidebar",
      chord: ["ctrlormeta+e", "ctrlormeta+\\"],
      run: () => {
        latestRef.current.toggleSidebar();
      },
    });
    bind({
      id: "tab-overview",
      label: "Tab overview",
      chord: "ctrlormeta+shift+a",
      run: () => {
        latestRef.current.toggleTabOverview();
      },
    });

    // Document commands. The modifier-tolerant ones keep their former behaviour of ignoring Shift/Alt.
    bind({
      id: "new-document",
      label: "New document",
      chord: "ctrlormeta+n",
      ignoreExtraModifiers: true,
      run: () => {
        latestRef.current.newDocument();
      },
    });
    bind({
      id: "open",
      label: "Open",
      chord: "ctrlormeta+o",
      ignoreExtraModifiers: true,
      run: () => {
        void latestRef.current.onOpenDialog();
      },
    });
    bind({
      id: "palette",
      label: "Command palette",
      chord: "ctrlormeta+shift+p",
      run: () => {
        latestRef.current.togglePalette();
      },
    });
    // Distraction-free reading is checked before the find handler so Shift+F doesn't open find.
    bind({
      id: "distraction-free",
      label: "Distraction-free reading",
      chord: "ctrlormeta+shift+f",
      run: () => {
        latestRef.current.toggleDistractionFree();
      },
    });

    // Find is always swallowed for Ctrl+F AND Ctrl+G so WebView2's native find-on-page never opens (Ctrl+G
    // is the engine's "find next" and would otherwise leak the native bar inside an iframe preview).
    // Route both to the host app own find; open even if the provider is still racing into place.
    const openFindFromShortcut = () => {
      const { canOpenFind, searchProvider, openFind } = latestRef.current;
      if (canOpenFind ?? !!searchProvider) openFind(false);
    };
    bind({
      id: "reset-zoom",
      label: "Reset zoom",
      chord: "ctrlormeta+0",
      ignoreExtraModifiers: true,
      when: hasActive,
      run: () => {
        latestRef.current.resetZoom();
      },
    });
    bind({
      id: "find",
      label: "Find",
      chord: "ctrlormeta+f",
      when: hasActive,
      run: openFindFromShortcut,
    });
    bind({
      id: "find-g",
      label: "Find",
      chord: "ctrlormeta+g",
      ignoreExtraModifiers: true,
      when: hasActive,
      run: openFindFromShortcut,
    });
    bind({
      id: "replace",
      label: "Replace",
      chord: "ctrlormeta+h",
      ignoreExtraModifiers: true,
      when: hasActive,
      run: () => {
        const { searchProvider, openFind } = latestRef.current;
        if (searchProvider?.canReplace) openFind(true);
      },
    });
    // Close the active document (with unsaved-guard) — never let WebView2 close the window.
    bind({
      id: "close-document",
      label: "Close document",
      chord: "ctrlormeta+w",
      ignoreExtraModifiers: true,
      when: hasActive,
      run: () => {
        const doc = latestRef.current.active;
        if (doc) latestRef.current.requestClose(doc.path);
      },
    });
    // DOCX prints through the editor bridge; route it like save so Ctrl+P works everywhere.
    bind({
      id: "print",
      label: "Print",
      chord: "ctrlormeta+p",
      when: hasActive,
      run: () => {
        const doc = latestRef.current.active;
        if (!doc) return;
        if (doc.format === "docx") void latestRef.current.docxMenu?.print();
        else latestRef.current.onPrint(doc);
      },
    });
    bind({
      id: "save-as",
      label: "Save as",
      chord: "ctrlormeta+shift+s",
      when: hasActive,
      run: () => {
        const doc = latestRef.current.active;
        if (doc && capabilitiesFor(doc.format).canSaveAs) latestRef.current.onSaveAs(doc);
      },
    });
    bind({
      id: "save",
      label: "Save",
      chord: "ctrlormeta+s",
      when: hasActive,
      run: () => {
        const doc = latestRef.current.active;
        if (!doc) return;
        // Route DOCX save through the editor bridge (it no longer ships its own toolbar/keymap).
        if (doc.format === "docx") void latestRef.current.docxMenu?.save();
        // One dirty page saves directly; several open the page-aware Save current / Save all chooser.
        else if (doc.format === "html") latestRef.current.onHtmlSaveShortcut(doc.path);
        else if (latestRef.current.activeEditable) latestRef.current.onSave(doc);
      },
    });

    const detachKeys = registry.attach(window);

    // Native Window-menu items can't bind ⌃Tab through the menu-bridge key syntax, so lib.rs emits
    // these events instead ("Select Next/Previous Tab").
    const nextTab = () => latestRef.current.cycleTab(1);
    const prevTab = () => latestRef.current.cycleTab(-1);
    window.addEventListener(documentEvent("next-tab"), nextTab);
    window.addEventListener(documentEvent("prev-tab"), prevTab);
    // The crash screen's "Close tab" action (ErrorBoundary) asks the shell to close a tab through
    // the same unsaved-guarded path as Ctrl+W.
    const closeTab = (e: Event) => {
      const path = (e as CustomEvent<{ path?: string }>).detail?.path;
      if (path) latestRef.current.requestClose(path);
    };
    window.addEventListener(documentEvent("close-tab"), closeTab);
    return () => {
      window.removeEventListener(documentEvent("close-tab"), closeTab);
      detachKeys();
      window.removeEventListener(documentEvent("next-tab"), nextTab);
      window.removeEventListener(documentEvent("prev-tab"), prevTab);
    };
  }, []);
}
