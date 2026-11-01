/**
 * Chords for "Ctrl, Meta or both, no Alt" shortcuts, for handlers that used to test
 * `(event.metaKey || event.ctrlKey)` by hand. The shortcut registry matches modifiers exactly, so the
 * three Ctrl/Meta combinations are listed; `shift` selects which Shift states are accepted.
 */
export function ctrlMetaChords(key: string, shift: readonly boolean[] = [false]): string[] {
  const chords: string[] = [];
  for (const withShift of shift) {
    for (const mods of ["ctrl", "meta", "ctrl+meta"]) {
      chords.push(`${mods}${withShift ? "+shift" : ""}+${key}`);
    }
  }
  return chords;
}
