import { useEffect, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

/**
 * Render children into the title bar's slot.
 *
 * The session header (title, workspace path, Compact, engine chip) is built in
 * AppBody, where its handlers and session state already live, but it belongs
 * *in* the window bar — the host app used to draw it as a second full-width row
 * directly underneath, which Codex does not do (Adrian, 2026-08-04). A portal
 * moves the rendered output without threading a dozen props up to AppFrame.
 *
 * The slot is owned by TitleBar, which mounts earlier in the tree, so it exists
 * by the time this effect runs. If it is ever missing — a test rendering
 * AppBody on its own, for instance — this renders nothing rather than throwing.
 */
export default function TitleBarSlotPortal({ children }: { children: ReactNode }) {
  const [slot, setSlot] = useState<Element | null>(null);

  useEffect(() => {
    setSlot(document.querySelector("[data-cr-titlebar-slot]"));
  }, []);

  if (!slot) return null;
  return createPortal(children, slot);
}
