import { useCallback, useState } from "react";
import { useMediaQuery } from "@rightkit/react-shell/react";

function initialSidebarCollapsed(compact: boolean) {
  if (typeof window === "undefined") return false;
  const pinned = localStorage.getItem("vr.sidebar.pinned");
  return pinned === "0" || compact;
}

/**
 * Overlay, panel, and dialog flags plus the Info-inspector callbacks. Extracted verbatim from useAppState;
 * runs right after the document core, matching the original call order.
 */
export function useAppChromeState() {
  const compactViewport = useMediaQuery("(max-width: 640px)");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [openUrlDialog, setOpenUrlDialog] = useState(false);
  // Titlebar tab-overview card grid (⌘⇧A / the strip's cards button).
  const [tabOverviewOpen, setTabOverviewOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => initialSidebarCollapsed(compactViewport));
  // Wave 3 Info inspector — one global open flag (per-document persistence is not required). The
  // toggle lives in the right toolbar capsule (DocumentTools); Escape inside the panel routes here,
  // and closeInspector returns focus to that toggle button.
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const closeInspector = useCallback(() => {
    setInspectorOpen(false);
    document.getElementById("vr-inspector-toggle")?.focus();
  }, []);
  const toggleInspector = useCallback(() => setInspectorOpen((o) => !o), []);
  return {
    settingsOpen,
    setSettingsOpen,
    paletteOpen,
    setPaletteOpen,
    helpOpen,
    setHelpOpen,
    openUrlDialog,
    setOpenUrlDialog,
    tabOverviewOpen,
    setTabOverviewOpen,
    sidebarCollapsed,
    setSidebarCollapsed,
    inspectorOpen,
    toggleInspector,
    closeInspector,
  };
}
