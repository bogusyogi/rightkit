import { cx } from "@rightkit/react-shell/react";
import { useEffect, useState } from "react";
import { getCurrentWebview } from "../lib/ipc/index.js";
import { nextDropOverlayVisible } from "../lib/dropOverlay.js";

/**
 * Full-window drop hint. Driven by Tauri's native drag-drop events, not HTML5
 * drag events: with native drag-drop enabled the webview never delivers the
 * HTML5 ones on Windows, so the overlay never showed (audit B9, #173).
 */
export function DropOverlay() {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent((e) => {
      setVisible((current) => nextDropOverlayVisible(current, e.payload.type));
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return (
    <div
      aria-hidden="true"
      className={cx(
        "pointer-events-none fixed inset-0 z-40 rounded-[8px] border-2 border-dashed border-[var(--accent)] bg-[color-mix(in_srgb,var(--accent)_10%,transparent)] opacity-0 transition-opacity duration-150",
        visible && "opacity-100",
      )}
    />
  );
}
