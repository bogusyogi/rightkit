/* Native WebView context-menu suppression. Touches only the document it is given; no native imports. */
type ContextMenuTarget = Pick<EventTarget, "addEventListener" | "removeEventListener">;

const installed = new WeakMap<ContextMenuTarget, () => void>();

/**
 * Suppress the native context menu on every `contextmenu` in `doc` (capture phase), so app chrome shows no
 * Inspect Element. Returns the uninstall function; calling again while installed returns the same one. With no
 * document (SSR, tests without DOM) it does nothing and returns a no-op.
 */
export function blockNativeContextMenu(doc: ContextMenuTarget | undefined = (globalThis as { document?: ContextMenuTarget }).document): () => void {
  if (!doc) return () => {};
  const existing = installed.get(doc);
  if (existing) return existing;
  const block = (event: Event) => event.preventDefault();
  doc.addEventListener("contextmenu", block, true);
  const uninstall = () => {
    doc.removeEventListener("contextmenu", block, true);
    installed.delete(doc);
  };
  installed.set(doc, uninstall);
  return uninstall;
}
