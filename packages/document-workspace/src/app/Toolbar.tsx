import { memo, type ReactNode } from "react";
import { createWindowControlLabels } from "@rightkit/platform-ui";
import { usePlatform } from "@rightkit/platform-ui/react";
import { Titlebar } from "@rightkit/react-shell/react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { inTauri } from "@rightkit/document-core/state/tauriBridge";
import { LiveTitle } from "./LiveTitle";
import { useTabDirty } from "./tabs/useTabDirty";
import { TabStrip, type TabStripProps } from "./tabs/TabStrip";

interface Props {
  doc: LoadedDocument | null;
  title: string;
  menuSlot?: ReactNode;
  modeSlot?: ReactNode;
  /** Centre-of-toolbar slot (Wave 2 Read/Edit control) — floats inside the drag region. */
  centerSlot?: ReactNode;
  sidebarCollapsed?: boolean;
  // When provided, the title stays live as you type an untitled doc (no App re-render). Falls back
  // to the static `title` string when absent (standalone renders/tests).
  compute?: (doc: LoadedDocument) => string;
  subscribeEdits?: (path: string, cb: () => void) => () => void;
  getEditSnapshot?: (path: string) => string | undefined;
  // Dirty-state wiring for the "— Edited" subtitle. subscribeDirty lets the subtitle track
  // text-buffer edits without re-rendering the App shell; dirtyPaths covers the non-text formats.
  // Absent (tests/standalone), the subtitle stays clean.
  isDirty?: (doc: LoadedDocument) => boolean;
  dirtyPaths?: ReadonlySet<string>;
  subscribeDirty?: (path: string, cb: () => void) => () => void;
  /** Titlebar tab strip (Part A tabs+explorer): when provided, the middle stretch renders
   *  open-document tabs instead of empty drag space, and the left title block is dropped —
   *  the strip's active tab carries the document name. Absent = standalone/test layout. */
  tabs?: TabStripProps;
}

const NO_DIRTY: ReadonlySet<string> = new Set();

// Subtitle kind labels — the human format names from the Wave 1 title-bar spec. Exported for the
// Wave 3 Info inspector, which must show the same "Kind" the title subtitle shows.
export const KIND_LABEL: Record<LoadedDocument["format"], string> = {
  md: "Markdown",
  mdx: "Markdown",
  txt: "Text",
  pdf: "PDF Document",
  docx: "Word Document",
  html: "HTML",
  image: "Image",
  web: "Web Preview",
  unknown: "Document",
};

const windowAdapter = {
  minimize: () => getCurrentWindow().minimize(),
  toggleMaximize: () => getCurrentWindow().toggleMaximize(),
  close: () => getCurrentWindow().close(),
};

const windowControlLabels = createWindowControlLabels({
  minimize: "Minimize",
  maximize: "Maximize",
  close: "Close",
});

// Title bar: document menus, the left-aligned document title block (name over kind/dirty subtitle),
// and window controls. On macOS the sidebar tells us when the native traffic lights are inside the
// content titlebar; only that collapsed state needs padding.
export const Toolbar = memo(ToolbarImpl);

function ToolbarImpl({
  doc,
  title,
  menuSlot,
  modeSlot,
  centerSlot,
  sidebarCollapsed = false,
  compute,
  subscribeEdits,
  getEditSnapshot,
  isDirty,
  dirtyPaths,
  subscribeDirty,
  tabs,
}: Props) {
  const mac = usePlatform() === "macos";
  // Collapsed-mac traffic-light clearance: standalone mode pads the left menu cluster;
  // with tabs, the strip's own leading cluster carries that inset instead.
  const menuPadding = mac && sidebarCollapsed && !tabs ? "pl-24" : "pl-3";
  const titleNode =
    doc && compute && subscribeEdits && getEditSnapshot ? (
      <LiveTitle
        doc={doc}
        compute={compute}
        subscribeEdits={subscribeEdits}
        getEditSnapshot={getEditSnapshot}
      />
    ) : (
      title
    );
  const hasTitle = !!(tabs || centerSlot);
  // `Titlebar` owns the drag-region header, the macOS traffic-light slot (hidden here: the sidebar,
  // or the tab strip's lead spacer, already clears the lights) and the Windows/Linux caption buttons
  // (drawn only in a native window; macOS has its own overlay title bar).
  return (
    <Titlebar
      className="no-print vr-chrome relative bg-panel"
      startWidth="auto"
      windowControls={inTauri() ? windowAdapter : undefined}
      windowControlLabels={windowControlLabels}
      slots={{
        side: `relative z-10 min-w-0 ${menuPadding}`,
        lights: "hidden",
        title: "relative h-full min-w-3 flex-1 self-stretch",
        drag: hasTitle ? "hidden" : "",
        end: "relative z-10",
      }}
      start={
        <>
          {/* With the sidebar open its titlebar hosts the in-window menus (far left, where macOS
              puts the traffic lights); here they show only when the sidebar is collapsed. */}
          {(!tabs || sidebarCollapsed) && menuSlot}
          {doc && !tabs ? (
            <ToolbarTitle
              doc={doc}
              titleNode={titleNode}
              isDirty={isDirty}
              dirtyPaths={dirtyPaths}
              subscribeDirty={subscribeDirty}
            />
          ) : null}
        </>
      }
      title={
        hasTitle ? (
          <>
            {tabs ? <TabStrip {...tabs} /> : null}
            {centerSlot ? (
              <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
                <div className="pointer-events-auto">{centerSlot}</div>
              </div>
            ) : null}
          </>
        ) : undefined
      }
      end={modeSlot ? <div className="mr-3 flex items-center">{modeSlot}</div> : undefined}
    />
  );
}

// Left-aligned document title: 15px semibold name over an 11px muted `<Kind> — Edited` subtitle.
// Subscribes to per-tab dirty changes so text-buffer edits flip "— Edited" without an App render.
function ToolbarTitle({
  doc,
  titleNode,
  isDirty,
  dirtyPaths,
  subscribeDirty,
}: {
  doc: LoadedDocument;
  titleNode: ReactNode;
  isDirty?: (doc: LoadedDocument) => boolean;
  dirtyPaths?: ReadonlySet<string>;
  subscribeDirty?: (path: string, cb: () => void) => () => void;
}) {
  const dirty = useTabDirty(
    doc,
    (dirtyPaths ?? NO_DIRTY).has(doc.path),
    isDirty ?? (() => false),
    subscribeDirty ?? (() => () => {}),
  );
  const kind = KIND_LABEL[doc.format] ?? "Document";
  return (
    <div className="ml-2 flex min-w-0 flex-col justify-center leading-tight" data-vr-doc-title>
      <span className="truncate font-sans text-[15px] font-semibold text-paper">{titleNode}</span>
      <span className="truncate font-sans text-chrome-xs text-muted">
        {kind}
        {dirty ? " — Edited" : ""}
      </span>
    </div>
  );
}
