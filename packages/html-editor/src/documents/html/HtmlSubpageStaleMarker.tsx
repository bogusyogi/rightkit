/**
 * Status pill shown in the sub-page address capsule when the file changed on disk while this page
 * had unsaved edits. The draft keeps its text but is based on the older bytes until it is reloaded
 * or saved.
 */
export function HtmlSubpageStaleMarker() {
  return (
    <span
      data-testid="html-subpage-stale"
      role="status"
      title="The file changed on disk while this page had unsaved edits. Reload or save to resolve."
      className="shrink-0 rounded-full border border-copper/50 bg-copper/15 px-2 py-0.5 text-chrome-xs font-semibold text-copper-hi"
    >
      <span className="sr-only">Stale draft: </span>
      Changed on disk
    </span>
  );
}
