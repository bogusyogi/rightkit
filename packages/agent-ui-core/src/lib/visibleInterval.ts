/**
 * An interval that does not run while the window is hidden or minimised (desktop R12). The main
 * window keeps `backgroundThrottling: "disabled"` so streaming stays live, which means a plain
 * `setInterval` would keep polling the daemon from a window nobody can see. A tick that fires
 * while hidden is skipped; one extra tick runs when the window becomes visible again.
 */
type VisibilityDocument = Pick<Document, "visibilityState" | "addEventListener" | "removeEventListener">;

export function setVisibleInterval(
  callback: () => void,
  ms: number,
  doc: VisibilityDocument = document,
): () => void {
  const timer = setInterval(() => {
    if (doc.visibilityState !== "hidden") callback();
  }, ms);
  const onVisibilityChange = () => {
    if (doc.visibilityState === "visible") callback();
  };
  doc.addEventListener("visibilitychange", onVisibilityChange);
  return () => {
    clearInterval(timer);
    doc.removeEventListener("visibilitychange", onVisibilityChange);
  };
}
