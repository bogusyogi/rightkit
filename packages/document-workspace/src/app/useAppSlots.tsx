import { useCallback, useMemo } from "react";
import { DocumentMenus, DocumentModeControls } from "./DocActionBar.js";
import type { LoadedDocument, ReadEditMode } from "@rightkit/document-core/documents/types";
import { capabilitiesFor } from "../documents/capabilities.js";
import type { useDocuments } from "./useDocuments.js";
import type { useFormatApis } from "./useFormatApis.js";
import type { useViewState } from "./useViewState.js";

type Docs = ReturnType<typeof useDocuments>;
type Formats = ReturnType<typeof useFormatApis>;

export interface AppSlotsInput {
  docs: Docs;
  active: LoadedDocument | null;
  view: Parameters<typeof DocumentMenus>[0]["view"];
  onChangeView: ReturnType<typeof useViewState>["requestViewChange"];
  handlePrint: Formats["handlePrint"];
  docxMenu: Formats["docxMenu"];
  activeReadEdit: ReadEditMode;
}

/**
 * Stable close/edit/open callbacks and the docked-bar menu + mode slots. Extracted verbatim from
 * useAppState; hook call order is unchanged.
 */
export function useAppSlots({
  docs,
  active,
  view,
  onChangeView,
  handlePrint,
  docxMenu,
  activeReadEdit,
}: AppSlotsInput) {
  const { onSave, onSaveAs, requestClose, open } = docs;
  // Stabilized chrome props so the memoized Sidebar/Toolbar/DocStatsPill skip the per-keystroke
  // App re-render — slots are memoized on the data they actually read.
  const closeActive = useCallback(() => {
    if (active) requestClose(active.path);
  }, [active, requestClose]);
  const editActiveImageExternal = useCallback(() => {
    if (active) docs.onEditImageExternal(active);
  }, [active, docs]);
  // T2-3: stable identity so the memoized DocumentRouterTable doesn't see a fresh function every render.
  const onOpenPathForRouter = useCallback(
    (p: string, opts?: { background?: boolean }) => {
      void open(p, opts);
    },
    [open],
  );
  const menuSlot = useMemo(
    () =>
      active ? (
        <DocumentMenus
          doc={active}
          view={view}
          onSave={() =>
            active.format === "html" ? docs.onHtmlSaveShortcut(active.path) : onSave(active)
          }
          onSaveAs={() => onSaveAs(active)}
          onPrint={() => handlePrint(active)}
          onClose={closeActive}
          onEditImageExternal={editActiveImageExternal}
          onChangeView={onChangeView}
          docxMenu={active.format === "docx" ? docxMenu : null}
          readOnly={activeReadEdit === "read"}
        />
      ) : null,
    [
      active,
      view,
      docxMenu,
      onChangeView,
      onSave,
      onSaveAs,
      handlePrint,
      docs,
      closeActive,
      editActiveImageExternal,
      activeReadEdit,
    ],
  );
  const modeSlot = useMemo(
    () =>
      active && capabilitiesFor(active.format).canSwitchView ? (
        <DocumentModeControls
          doc={active}
          view={view}
          onChangeView={onChangeView}
          readOnly={activeReadEdit === "read"}
        />
      ) : null,
    [active, view, onChangeView, activeReadEdit],
  );
  return { onOpenPathForRouter, menuSlot, modeSlot };
}
