/**
 * Stale-hover guard for WebView2 (Windows) and other webviews that keep `:hover` after the pointer leaves the window.
 * It sets `data-rk-pointer="on"` while the pointer moves over the document and `"off"` when it leaves the window, the
 * window blurs or the page is hidden. The shell's hover washes (`:root:not([data-rk-pointer="off"])` selectors in
 * `shell.visual.css`) then apply only while the pointer is really over the window. Without the guard the attribute is
 * absent and hover behaves as before. Returns the detach function, which also removes the attribute.
 */
export function installHoverGuard(root: HTMLElement | null = typeof document === "undefined" ? null : document.documentElement): () => void {
  if (!root || typeof window === "undefined" || typeof document === "undefined") return () => undefined;
  const set = (value: "on" | "off") => root.setAttribute("data-rk-pointer", value);
  const onMove = () => set("on");
  const onOut = (event: Event) => {
    // A pointerout with no related target means the pointer left the window.
    if (!(event as PointerEvent).relatedTarget) set("off");
  };
  const onBlur = () => set("off");
  const onVisibility = () => {
    if (document.visibilityState !== "visible") set("off");
  };
  window.addEventListener("pointermove", onMove, { passive: true });
  document.addEventListener("pointerout", onOut);
  window.addEventListener("blur", onBlur);
  document.addEventListener("visibilitychange", onVisibility);
  return () => {
    window.removeEventListener("pointermove", onMove);
    document.removeEventListener("pointerout", onOut);
    window.removeEventListener("blur", onBlur);
    document.removeEventListener("visibilitychange", onVisibility);
    root.removeAttribute("data-rk-pointer");
  };
}
