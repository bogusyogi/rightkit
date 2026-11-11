// The editing glyphs this package adds to the shared set of @rightkit/react-shell (`Icon`, `ICONS`).
// Paths from Lucide (ISC License, see the notice in @rightkit/react-shell's icons).
import { Icon as SharedIcon, type IconName as SharedIconName, type IconProps, type IconShape } from "@rightkit/react-shell/react";

const p = (...d: string[]): IconShape => d.map((path) => ["path", { d: path }] as const);

/** Editing glyphs that are not in the shared set. */
export const EDITING_GLYPHS = {
  "layout-list": p(
    "M4 3h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1Z",
    "M4 14h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1Z",
    "M14 4h7",
    "M14 9h7",
    "M14 15h7",
    "M14 20h7",
  ),
  film: p("M3 3h18v18H3z", "M7 3v18", "M17 3v18", "M3 7.5h4", "M3 12h18", "M3 16.5h4", "M17 7.5h4", "M17 16.5h4"),
  scissors: p(
    "M6 3a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z",
    "M6 15a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z",
    "M20 4 8.12 15.88",
    "M14.47 14.48 20 20",
    "M8.12 8.12 12 12",
  ),
  "mouse-pointer": p(
    "M12.586 12.586 19 19",
    "M3.688 3.037a.497.497 0 0 0-.651.651l6.5 15.999a.501.501 0 0 0 .947-.062l1.569-6.083a2 2 0 0 1 1.448-1.479l6.124-1.579a.5.5 0 0 0 .063-.947z",
  ),
  "step-back": p("M17 18 9 12l8-6Z", "M7 6v12"),
  "step-forward": p("m7 18 8-6-8-6Z", "M17 6v12"),
  repeat: p("m17 2 4 4-4 4", "M3 11v-1a4 4 0 0 1 4-4h14", "m7 22-4-4 4-4", "M21 13v1a4 4 0 0 1-4 4H3"),
  magnet: p("m6 15-4-4 6.75-6.77a7.79 7.79 0 0 1 11 11L13 22l-4-4 6.39-6.36a2.14 2.14 0 0 0-3-3L6 15", "m5 8 4 4", "m12 15 4 4"),
  captions: p("M3 5h18v14H3z", "M7 15h4", "M15 15h2", "M7 11h2", "M13 11h4"),
} as const satisfies Record<string, IconShape>;

export type EditingGlyph = keyof typeof EDITING_GLYPHS;
export type IconName = SharedIconName | EditingGlyph;

/** The shared icon with the editing glyphs added. */
export function Icon(props: Omit<IconProps<EditingGlyph>, "glyphs">) {
  return <SharedIcon<EditingGlyph> {...props} glyphs={EDITING_GLYPHS} />;
}
