// The glyphs the app pages use beyond @rightkit/react-shell's shared set and the editing glyphs (`../../ui/icons`).
// Paths from Lucide (ISC License, see the notice in @rightkit/react-shell's icons).
import { Icon as SharedIcon, type IconName as SharedIconName, type IconProps, type IconShape } from "@rightkit/react-shell/react";
import { EDITING_GLYPHS, type EditingGlyph } from "../../ui/icons.js";

const p = (...d: string[]): IconShape => d.map((path) => ["path", { d: path }] as const);

/** App-page glyphs that are in neither the shared set nor `EDITING_GLYPHS`. */
export const APP_GLYPHS = {
  "sliders-horizontal": p("M21 4h-7", "M10 4H3", "M21 12h-9", "M8 12H3", "M21 20h-5", "M12 20H3", "M14 2v4", "M8 10v4", "M16 18v4"),
  "list-video": p("M12 12H3", "M16 6H3", "M12 18H3", "m16 12 5 3-5 3v-6Z"),
  "text-quote": p("M17 6H3", "M21 12H8", "M21 18H8", "M3 12v6"),
  "chart-line": p("M3 3v16a2 2 0 0 0 2 2h16", "m19 9-5 5-4-4-3 3"),
  "wand-sparkles": p(
    "m21.64 3.64-1.28-1.28a1.21 1.21 0 0 0-1.72 0L2.36 18.64a1.21 1.21 0 0 0 0 1.72l1.28 1.28a1.2 1.2 0 0 0 1.72 0L21.64 5.36a1.2 1.2 0 0 0 0-1.72",
    "m14 7 3 3",
    "M5 6v4",
    "M19 14v4",
    "M10 2v2",
    "M7 8H3",
    "M21 16h-4",
    "M11 3H9",
  ),
  clapperboard: p(
    "M20.2 6 3 11l-.9-2.4c-.3-1.1.3-2.2 1.3-2.5l13.5-4c1.1-.3 2.2.3 2.5 1.3Z",
    "m6.2 5.3 3.1 3.9",
    "m12.4 3.4 3.1 4",
    "M3 11h18v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z",
  ),
} as const satisfies Record<string, IconShape>;

export type AppGlyph = keyof typeof APP_GLYPHS | EditingGlyph;
export type AppIconName = SharedIconName | AppGlyph;

const GLYPHS: Record<AppGlyph, IconShape> = { ...EDITING_GLYPHS, ...APP_GLYPHS };

/** The shared icon with the editing and app-page glyphs added. */
export function Icon(props: Omit<IconProps<AppGlyph>, "glyphs">) {
  return <SharedIcon<AppGlyph> {...props} glyphs={GLYPHS} />;
}
