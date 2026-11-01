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

/** The browser's key event as a shortcut ("ctrl+opt+left"), or null when it
 *  names no key or has no modifier. Matches the names in WindowShortcut.swift. */
export function windowShortcutFrom(e: KeyboardEvent): string | null {
  const names: Record<string, string> = {
    ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down",
    Enter: "return", NumpadEnter: "return", Space: "space", Backspace: "delete", Tab: "tab",
    Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Comma: ",", Period: ".",
    Slash: "/", Semicolon: ";", Quote: "'", Backslash: "\\", Backquote: "`",
  };
  let key = names[e.code] ?? "";
  if (!key) {
    const match = /^(?:Key|Digit)([A-Z0-9])$/.exec(e.code);
    key = match ? match[1].toLowerCase() : "";
  }
  if (!key) return null;
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("ctrl");
  if (e.altKey) mods.push("opt");
  if (e.shiftKey) mods.push("shift");
  if (e.metaKey) mods.push("cmd");
  if (mods.length === 0) return null;
  return [...mods, key].join("+");
}

/** The "Open <section>" button's text while searching. */
export const openSectionLabel = (title: string) => `Open ${title}`;
