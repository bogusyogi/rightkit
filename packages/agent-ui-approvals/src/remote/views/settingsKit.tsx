/**
 * the host app's own pieces of the Settings look. Groups, rows, items, sections, the search provider and
 * the search state (`useSettingsSearch`) are @rightkit/settings-ui; this file keeps only what
 * settings-ui does not draw.
 */

/** Under a Permissions row: what switched-on features need it. Nothing when the notch does not say. */
export function NeedsLine({ needs }: { needs?: string[] }) {
  if (!needs) return null;
  return (
    <div className="ck-sub ck-foot ck-needs">
      {needs.length > 0 ? `Needed for: ${needs.join(" · ")}` : "Not needed by anything you have on"}
    </div>
  );
}

/** The "Open <section>" button's text while searching. */
export const openSectionLabel = (title: string) => `Open ${title}`;
