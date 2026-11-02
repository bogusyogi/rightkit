import type { ComponentProps, ReactNode } from "react";
import { tabDomId } from "./tabs/tabIds.js";
import { DocStatsPill } from "./shell/DocStatsPill.js";
import { DocumentTools } from "./shell/DocumentTools.js";
import { DocBarSlotContext } from "@rightkit/document-core/app/shell/docBar";
import { InfoInspector } from "./shell/InfoInspector.js";
import { StartPage } from "./shell/StartPage.js";
import { OpeningProgress } from "./OpeningProgress.js";
import { BlockDraftProvider } from "@rightkit/document-core/app/BlockDraftContext";
import { DocumentRouterTable } from "./DocumentRouterTable.js";
import type { useSettings } from "@rightkit/document-core/settings/settings";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { DocxMenuApi } from "@rightkit/docx-editor/documents/docx/menuApi";

type StatsProps = ComponentProps<typeof DocStatsPill>;
type RouterProps = ComponentProps<typeof DocumentRouterTable>;

export type DocumentPaneProps = {
  active: LoadedDocument | null;
  activeIndex: number;
  activeEditable: boolean;
  view: ComponentProps<typeof DocumentTools>["view"];
  distractionFree: boolean;
  docBarSlot: HTMLElement | null;
  setDocBarSlot: (el: HTMLElement | null) => void;
  modeSlot: ReactNode;
  inspectorOpen: boolean;
  toggleInspector: () => void;
  closeInspector: () => void;
  findBarEl: ReactNode;
  zoom: {
    zoomHud: ReactNode;
    zoomedAwayFromDefault: boolean;
    zoomPercentLabel: string;
    resetDocumentZoom: () => void;
  };
  openingPath: string | null;
  restoringSession: boolean;
  openDocs: LoadedDocument[];
  startPage: ComponentProps<typeof StartPage>;
  blockDrafts: ComponentProps<typeof BlockDraftProvider>["registry"];
  router: RouterProps;
  subscribeEdits: StatsProps["subscribeEdits"];
  getEditSnapshot: StatsProps["getEditSnapshot"];
  dirtyPaths: Set<string>;
  isDirty: (doc: LoadedDocument) => boolean;
  subscribeDirty: (path: string, listener: () => void) => () => void;
  docxMenu: DocxMenuApi | null;
  onHtmlSave: (path: string) => unknown;
  onSave: (doc: LoadedDocument) => unknown;
  settings: ReturnType<typeof useSettings>["settings"];
  update: ReturnType<typeof useSettings>["update"];
};

/** The document column: docked format bar, the content plane (find bar, zoom pill, opening/restore
 * indicators, start page, router table), the status row, and the Info inspector. Extracted
 * verbatim from App.tsx. */
export function DocumentPane(p: DocumentPaneProps) {
  const { active, distractionFree, zoom } = p;
  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      {/* Search belongs below the docked bar, independent of its centre column. */}
      <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
        {/* The page's curved top-left corner belongs to the top of the page — the docked bar. */}
        <svg data-vr-content-corner viewBox="0 0 24 24" aria-hidden="true" focusable="false">
          <path d="M0 24V0H24A24 24 0 0 0 0 24Z" />
        </svg>
        {/* One quiet toolbar row (polish pass): the active format's tools on the left —
            portalled in via <DocBar> and wrapping to extra rows at narrow widths — the view
            tools (mode switch, Aa, info) on the right. No nested capsules; the bottom status
            row carries path + stats + save state. Content starts below. */}
        {active && !distractionFree && (
          <div
            data-vr-doc-bar
            className="no-print relative z-30 flex shrink-0 items-center gap-2 border-b border-[color-mix(in_srgb,var(--reader-line)_62%,transparent)] bg-[var(--reader-surface)] py-1 pl-3.5 pr-3"
          >
            <div ref={p.setDocBarSlot} className="flex min-w-0 flex-1 items-center" />
            <div className="flex shrink-0 items-center gap-1.5">
              {p.modeSlot}
              {p.modeSlot && <span aria-hidden="true" className="mx-0.5 h-4 w-px bg-line" />}
              <DocumentTools
                doc={active}
                view={p.view}
                inspectorOpen={p.inspectorOpen}
                onToggleInspector={p.toggleInspector}
              />
            </div>
          </div>
        )}
        <DocBarSlotContext.Provider value={distractionFree ? null : p.docBarSlot}>
          <div
            data-vr-content-plane
            id="vr-doc-pane"
            role="tabpanel"
            aria-labelledby={active ? tabDomId(active) : undefined}
            className="relative min-h-0 min-w-0 flex-1 overflow-hidden bg-[var(--reader-surface)]"
          >
            {p.findBarEl && (
              <div
                data-vr-toolbar-find=""
                className="no-print vr-chrome absolute top-2 z-50 overflow-y-auto max-h-[calc(100%-1rem)]"
              >
                {p.findBarEl}
              </div>
            )}
            {/* While zoom is off 100% the pill STAYS and becomes the reset control. Cmd/Ctrl+0 has
          always reset zoom, but nothing advertised it, so a mis-scrolled document looked stuck.
          At 100% it reverts to the old transient read-out and stops taking pointer events. */}
            {zoom.zoomedAwayFromDefault ? (
              <button
                type="button"
                onClick={zoom.resetDocumentZoom}
                aria-label="Reset zoom to 100%"
                className="no-print absolute right-4 top-4 z-50 rounded border border-line bg-panel-2 px-2 py-1 font-mono text-xs text-muted shadow-xl hover:text-fg"
              >
                {zoom.zoomPercentLabel} · Reset
              </button>
            ) : (
              zoom.zoomHud && (
                <div className="no-print pointer-events-none absolute right-4 top-4 z-50 rounded border border-line bg-panel-2 px-2 py-1 font-mono text-xs text-muted shadow-xl">
                  {zoom.zoomHud}
                </div>
              )
            )}
            {p.openingPath && (
              <OpeningProgress
                key={p.openingPath}
                fileName={p.openingPath.split(/[\\/]/).pop() || "document"}
              />
            )}
            {/* Restore indicator: only show once the launch effect has populated tabs (openDocs
          non-empty) but no tab is active yet. Before that (initial mount, no saved session)
          the StartPage is the right thing — the launch effect flips restoringSession true
          on entry and false in `finally`. Gating on openDocs.length > 0 keeps the existing
          "fresh launch with nothing to restore" path showing StartPage immediately, so
          its interactions (New document, Open URL, Paste) keep working. */}
            {!active && p.restoringSession && p.openDocs.length > 0 && (
              <div
                role="status"
                aria-live="polite"
                data-testid="restoring-session"
                className="absolute inset-0 flex items-center justify-center bg-[var(--reader-surface)] outline-none"
              >
                <div className="flex items-center gap-3 font-sans text-sm text-muted">
                  <span className="h-2 w-2 animate-pulse rounded-full bg-copper" aria-hidden />
                  Restoring session…
                </div>
              </div>
            )}
            {!active && (!p.restoringSession || p.openDocs.length === 0) && (
              <div className="absolute inset-0 overflow-auto bg-[var(--reader-surface)] outline-none">
                <StartPage {...p.startPage} />
              </div>
            )}
            {/* One scroll pane per MOUNTED tab. Inactive panes stay in layout with visibility:hidden —
          React keeps their state (DOM, iframe, scroll), and WebView2 doesn't discard srcdoc
          iframes the way it can on display:none transitions. Only the active pane owns the
          shared scroll ref + wheel/scroll handlers. */}
            <BlockDraftProvider registry={p.blockDrafts}>
              <DocumentRouterTable {...p.router} />
            </BlockDraftProvider>
          </div>
        </DocBarSlotContext.Provider>
        {/* Bottom status row: file path (left) + doc stats / save / autosave (right).
            Outside the scrolling content plane, docked to the page's bottom edge. */}
        {active && !distractionFree && (
          <div data-vr-doc-status-bar className="no-print shrink-0 justify-between font-sans">
            <div className="vr-doc-path min-w-0 flex-1 opacity-80">{active.path}</div>
            <DocStatsPill
              doc={active}
              subscribeEdits={p.subscribeEdits}
              getEditSnapshot={p.getEditSnapshot}
              status={
                p.activeEditable
                  ? {
                      dirtyHint: p.dirtyPaths.has(active.path),
                      isDirty: p.isDirty,
                      subscribeDirty: p.subscribeDirty,
                      onSave: () =>
                        active.format === "docx" && p.docxMenu
                          ? p.docxMenu.save()
                          : active.format === "html"
                            ? p.onHtmlSave(active.path)
                            : p.onSave(active),
                      autoSave: p.settings.autoSave,
                      onToggleAutoSave: () => p.update({ autoSave: !p.settings.autoSave }),
                    }
                  : undefined
              }
            />
          </div>
        )}
      </div>
      {p.inspectorOpen && active && !distractionFree && (
        <InfoInspector
          doc={active}
          onClose={p.closeInspector}
          subscribeEdits={p.subscribeEdits}
          getEditSnapshot={p.getEditSnapshot}
        />
      )}
    </div>
  );
}
