import { createElement, useMemo, type ComponentType } from "react";
import { File } from "@phosphor-icons/react/dist/icons/File";
import { ClockCounterClockwise } from "@phosphor-icons/react/dist/icons/ClockCounterClockwise";
import { FilePlus } from "@phosphor-icons/react/dist/icons/FilePlus";
import { FolderOpen } from "@phosphor-icons/react/dist/icons/FolderOpen";
import { GlobeSimple } from "@phosphor-icons/react/dist/icons/GlobeSimple";
import { ArrowClockwise } from "@phosphor-icons/react/dist/icons/ArrowClockwise";
import { Gear } from "@phosphor-icons/react/dist/icons/Gear";
import { Keyboard } from "@phosphor-icons/react/dist/icons/Keyboard";
import { CircleHalf } from "@phosphor-icons/react/dist/icons/CircleHalf";
import { TextAlignLeft } from "@phosphor-icons/react/dist/icons/TextAlignLeft";
import { Palette } from "@phosphor-icons/react/dist/icons/Palette";
import { Copy } from "@phosphor-icons/react/dist/icons/Copy";
import { ClipboardText } from "@phosphor-icons/react/dist/icons/ClipboardText";
import { Export } from "@phosphor-icons/react/dist/icons/Export";
import { FloppyDisk } from "@phosphor-icons/react/dist/icons/FloppyDisk";
import { Printer } from "@phosphor-icons/react/dist/icons/Printer";
import { Eye } from "@phosphor-icons/react/dist/icons/Eye";
import { Code } from "@phosphor-icons/react/dist/icons/Code";
import { capabilitiesFor } from "../documents/capabilities";
import { displayTitle, type LoadedDocument, type ViewMode } from "@rightkit/document-core/documents/types";
import { BASE_THEME_IDS, themeById } from "@rightkit/document-core/settings/themes";
import type { FileEntry } from "../state/library";
import type { Settings } from "@rightkit/document-core/settings/settings";
import type { DocxMenuApi } from "@rightkit/docx-editor/documents/docx/menuApi";
import { shortcutLabel } from "@rightkit/document-core/platform";
import type { PaletteItem } from "./CommandPalette";

// Decorative leading glyph for a palette row. Kept tiny + weight-regular to match the compact list.
// biome-ignore lint/suspicious/noExplicitAny: Phosphor's icon props are structurally identical here.
const ic = (I: ComponentType<any>) => createElement(I, { size: 16, weight: "regular" });

interface PaletteDeps {
  openDocs: LoadedDocument[];
  recents: FileEntry[];
  active: LoadedDocument | null;
  activeEditable: boolean;
  titleFor: (doc: LoadedDocument) => string;
  activate: (path: string) => void | Promise<void>;
  openRecent: (entry: FileEntry) => undefined | Promise<unknown>;
  newDocument: () => void;
  onOpenDialog: () => void | Promise<void>;
  onOpenUrl: () => void;
  onSave: (doc: LoadedDocument) => undefined | Promise<unknown>;
  onSaveAs: (doc: LoadedDocument) => undefined | Promise<unknown>;
  onPrint: (doc: LoadedDocument | null) => void;
  docxMenu: DocxMenuApi | null;
  onChangeView: (next: ViewMode) => void | Promise<void>;
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  openSettings: () => void;
  reopenClosed: () => void;
  showHelp: () => void;
  /** Copy the active document's text (text-like formats only); null/undefined hides the command. */
  copyContents: (() => void) | null;
  /** Export the active Markdown doc to HTML / plain text; null hides the commands. */
  exportMarkdown: ((target: "html" | "txt") => void) | null;
  /** Copy the active Markdown's RENDERED output to the clipboard (rich HTML + text); null hides it. */
  copyRendered: (() => void) | null;
  /** Copy a PDF/DOCX converted to Markdown; null hides it. */
  copyAsMarkdown: (() => void) | null;
  /** Duplicate the active text doc into a new unsaved draft; null hides the command. */
  duplicate: (() => void) | null;
}

/**
 * Command-palette entries: open tabs + recents + context-aware commands. Routes through the
 * existing document handlers — the palette is just a uniform, cross-format way to reach them.
 * Format-specific commands (save/print/view) gate on `capabilitiesFor`, not ad-hoc format checks.
 */
export function useCommandPalette(d: PaletteDeps): PaletteItem[] {
  const {
    openDocs,
    recents,
    active,
    activeEditable,
    titleFor,
    activate,
    openRecent,
    newDocument,
    onOpenDialog,
    onOpenUrl,
    onSave,
    onSaveAs,
    onPrint,
    docxMenu,
    onChangeView,
    settings,
    update,
    openSettings,
    reopenClosed,
    showHelp,
    copyContents,
    exportMarkdown,
    copyRendered,
    copyAsMarkdown,
    duplicate,
  } = d;
  return useMemo<PaletteItem[]>(() => {
    const tabItems: PaletteItem[] = openDocs.map((doc) => ({
      id: `tab:${doc.path}`,
      label: titleFor(doc),
      hint: "open tab",
      icon: ic(File),
      run: () => void activate(doc.path),
    }));
    const recentItems: PaletteItem[] = recents.map((r) => ({
      id: `recent:${r.token}`,
      label: displayTitle(r.displayName),
      hint: "recent",
      icon: ic(ClockCounterClockwise),
      run: () => void openRecent(r),
    }));
    const cmds: PaletteItem[] = [
      {
        id: "cmd:new",
        label: "New text document",
        hint: shortcutLabel("mod+n"),
        icon: ic(FilePlus),
        run: newDocument,
      },
      {
        id: "cmd:open",
        label: "Open file…",
        hint: shortcutLabel("mod+o"),
        icon: ic(FolderOpen),
        run: () => void onOpenDialog(),
      },
      {
        id: "cmd:open-url",
        label: "Open URL…",
        hint: "web",
        icon: ic(GlobeSimple),
        run: onOpenUrl,
      },
      {
        id: "cmd:reopen",
        label: "Reopen closed tab",
        hint: shortcutLabel("mod+shift+t"),
        icon: ic(ArrowClockwise),
        run: reopenClosed,
      },
      { id: "cmd:settings", label: "Settings", icon: ic(Gear), run: openSettings },
      {
        id: "cmd:shortcuts",
        label: "Keyboard shortcuts",
        hint: "?",
        icon: ic(Keyboard),
        run: showHelp,
      },
      {
        id: "cmd:contrast",
        label: `Contrast: ${settings.contrast === "high" ? "Normal" : "High"}`,
        hint: "typography",
        icon: ic(CircleHalf),
        run: () => update({ contrast: settings.contrast === "high" ? "normal" : "high" }),
      },
      {
        id: "cmd:wrap",
        label: `Word wrap: ${settings.wordWrap ? "Off" : "On"}`,
        hint: "editor",
        icon: ic(TextAlignLeft),
        run: () => update({ wordWrap: !settings.wordWrap }),
      },
      ...BASE_THEME_IDS.map((id) => {
        const theme = themeById(id);
        return {
          id: `cmd:theme-${id}`,
          label: `Theme: ${theme.name}`,
          hint: "theme",
          icon: ic(Palette),
          run: () => update({ themeId: id }),
        };
      }),
    ];
    if (active) {
      const c = capabilitiesFor(active.format);
      if (copyContents)
        cmds.push({
          id: "cmd:copy-contents",
          label: "Copy document contents",
          hint: "clipboard",
          icon: ic(Copy),
          run: copyContents,
        });
      if (copyRendered)
        cmds.push({
          id: "cmd:copy-rendered",
          label: "Copy rendered Markdown (for sharing)",
          hint: "clipboard",
          icon: ic(ClipboardText),
          run: copyRendered,
        });
      if (copyAsMarkdown)
        cmds.push({
          id: "cmd:copy-as-markdown",
          label: "Copy as Markdown",
          hint: "clipboard",
          icon: ic(ClipboardText),
          run: copyAsMarkdown,
        });
      if (duplicate)
        cmds.push({
          id: "cmd:duplicate",
          label: "Duplicate as draft",
          hint: "new copy",
          icon: ic(Copy),
          run: duplicate,
        });
      if (exportMarkdown) {
        cmds.push({
          id: "cmd:export-html",
          label: "Export as HTML…",
          hint: "markdown",
          icon: ic(Export),
          run: () => exportMarkdown("html"),
        });
        cmds.push({
          id: "cmd:export-txt",
          label: "Export as plain text…",
          hint: "markdown",
          icon: ic(Export),
          run: () => exportMarkdown("txt"),
        });
      }
      if (activeEditable) {
        cmds.push({
          id: "cmd:save",
          label: "Save",
          hint: shortcutLabel("mod+s"),
          icon: ic(FloppyDisk),
          run: () => (active.format === "docx" ? void docxMenu?.save() : void onSave(active)),
        });
      }
      if (c.canSaveAs) {
        cmds.push({
          id: "cmd:saveas",
          label: "Save As…",
          hint: shortcutLabel("mod+shift+s"),
          icon: ic(FloppyDisk),
          run: () => void onSaveAs(active),
        });
      }
      if (c.canPrintInApp)
        cmds.push({
          id: "cmd:print",
          label: "Print",
          hint: shortcutLabel("mod+p"),
          icon: ic(Printer),
          run: () => onPrint(active),
        });
      else if (active.format === "docx")
        cmds.push({
          id: "cmd:print",
          label: "Print",
          hint: shortcutLabel("mod+p"),
          icon: ic(Printer),
          run: () => void docxMenu?.print(),
        });
      if (c.canSwitchView) {
        if (active.format === "html") {
          cmds.push({
            id: "cmd:view-interactive",
            label: "View: Interactive",
            icon: ic(Eye),
            run: () => void onChangeView("interactive"),
          });
          cmds.push({
            id: "cmd:view-safe",
            label: "View: Safe",
            icon: ic(Eye),
            run: () => void onChangeView("preview"),
          });
        } else {
          cmds.push({
            id: "cmd:view-preview",
            label: "View: Preview",
            icon: ic(Eye),
            run: () => void onChangeView("preview"),
          });
          cmds.push({
            id: "cmd:view-source",
            label: "View: Source",
            icon: ic(Code),
            run: () => void onChangeView("source"),
          });
        }
      }
    }
    return [...tabItems, ...recentItems, ...cmds];
  }, [
    openDocs,
    recents,
    active,
    activeEditable,
    titleFor,
    activate,
    openRecent,
    newDocument,
    onOpenDialog,
    onOpenUrl,
    onSave,
    onSaveAs,
    onPrint,
    docxMenu,
    onChangeView,
    settings,
    update,
    openSettings,
    reopenClosed,
    showHelp,
    copyContents,
    exportMarkdown,
    duplicate,
    copyRendered,
    copyAsMarkdown,
  ]);
}
