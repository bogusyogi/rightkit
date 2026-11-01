import { openDevtools } from "@rightkit/document-core/state/tauriBridge";
import type { HtmlInspectTarget } from "./sourceMap.js";
import { useRef } from "react";
import { floatingPanelStyle, useDismiss } from "@rightkit/platform-ui/headless";

/**
 * The Safe-mode context menu overlay: Copy / Open link in new tab / Copy link / Copy image address /
 * View source / Open DevTools. This is the interaction surface for jumping from rendered content back
 * into the editable source drawer. Extracted verbatim from HtmlDocument — no behavior change.
 *
 * (Interactive mode uses WebView2's native context menu instead — a grafted "View source" item; see
 * context_menu.rs and docs/HTML_SECURITY_INVARIANTS.md. This component is the Safe-mode equivalent.)
 */
export function HtmlDocumentEdit({
  inspectMenu,
  onClose,
  onCopyValue,
  onOpenLinkNewTab,
  onViewSource,
}: {
  inspectMenu: { target: HtmlInspectTarget; x: number; y: number };
  onClose: () => void;
  onCopyValue: (value: string) => void;
  onOpenLinkNewTab: (href: string) => void;
  onViewSource: () => void;
}) {
  // The menu is mounted exactly while it is open. Escape and a window resize dismiss it; a click
  // whose target is the scrim (anywhere outside the panel) dismisses it too. Escape is not
  // swallowed and no default is prevented, as before.
  const scrimRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  useDismiss(() => onClose(), {
    outside: "click-target",
    container: panelRef,
    backdrop: scrimRef,
    onResize: true,
    preventDefault: false,
    stopPropagation: false,
    imeGuard: false,
  });
  const position = floatingPanelStyle({
    x: inspectMenu.x,
    y: inspectMenu.y,
    width: 176,
    estimatedHeight: 260,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
  });
  return (
    <>
      <div ref={scrimRef} className="no-print fixed inset-0 z-[79] cursor-default" aria-hidden />
      <div
        ref={panelRef}
        role="menu"
        aria-label="HTML actions"
        className="no-print fixed z-[80] overflow-x-hidden overflow-y-auto rounded-md border border-line bg-panel p-0.5 text-xs shadow-2xl"
        style={position}
        onMouseDown={(event) => event.stopPropagation()}
        onClick={(event) => event.stopPropagation()}
      >
        <button
          type="button"
          className="vr-press vr-focus block w-full rounded px-2.5 py-1 text-left text-muted hover:bg-panel-2 hover:text-paper"
          onClick={() => onCopyValue(inspectMenu.target.selectedText || inspectMenu.target.text)}
        >
          Copy
        </button>
        {inspectMenu.target.href && (
          <>
            <button
              type="button"
              className="vr-press vr-focus block w-full rounded px-2.5 py-1 text-left text-muted hover:bg-panel-2 hover:text-paper"
              onClick={() => {
                const href = inspectMenu.target.href;
                if (href) onOpenLinkNewTab(href);
              }}
            >
              Open link in new tab
            </button>
            <button
              type="button"
              className="vr-press vr-focus block w-full rounded px-2.5 py-1 text-left text-muted hover:bg-panel-2 hover:text-paper"
              onClick={() => onCopyValue(inspectMenu.target.href ?? "")}
            >
              Copy link address
            </button>
          </>
        )}
        {inspectMenu.target.src && (
          <button
            type="button"
            className="vr-press vr-focus block w-full rounded px-2.5 py-1 text-left text-muted hover:bg-panel-2 hover:text-paper"
            onClick={() => onCopyValue(inspectMenu.target.src ?? "")}
          >
            Copy image address
          </button>
        )}
        {/* Safe-mode menu (scriptless render). "View source" = jump to this element in the host app
            editable, save-to-file source drawer. "Open DevTools" opens the Chromium inspector.
            (Interactive mode uses WebView2's native menu instead — native Inspect + a grafted
            "View source" item; see context_menu.rs.) */}
        <button
          type="button"
          className="vr-press vr-focus block w-full rounded px-2.5 py-1 text-left text-muted hover:bg-panel-2 hover:text-paper"
          onClick={onViewSource}
        >
          View source
        </button>
        <button
          type="button"
          className="vr-press vr-focus block w-full rounded px-2.5 py-1 text-left text-muted hover:bg-panel-2 hover:text-paper"
          onClick={() => {
            onClose();
            void openDevtools();
          }}
        >
          Open DevTools
        </button>
      </div>
    </>
  );
}
