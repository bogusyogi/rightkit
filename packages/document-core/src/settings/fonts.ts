export interface BundledFont {
  id: string;
  name: string;
  /** CSS font-family value. */
  stack: string;
  kind: "serif" | "sans" | "mono";
}

/** Fonts shipped in /public/fonts (offline). "System…" is handled separately via a free-text family. */
export const BUNDLED_FONTS: BundledFont[] = [
  { id: "sentient", name: "Sentient", stack: "'Sentient', Georgia, serif", kind: "serif" },
  {
    id: "source-serif",
    name: "Source Serif 4",
    stack: "'Source Serif 4', Georgia, serif",
    kind: "serif",
  },
  { id: "newsreader", name: "Newsreader", stack: "'Newsreader', Georgia, serif", kind: "serif" },
  { id: "switzer", name: "Switzer", stack: "'Switzer', system-ui, sans-serif", kind: "sans" },
  { id: "inter", name: "Inter", stack: "'Inter', system-ui, sans-serif", kind: "sans" },
  { id: "geist", name: "Geist", stack: "'Geist', system-ui, sans-serif", kind: "sans" },
  {
    id: "spline-sans-mono",
    name: "Spline Sans Mono",
    stack: "'Spline Sans Mono', ui-monospace, monospace",
    kind: "mono",
  },
  {
    id: "jetbrains",
    name: "JetBrains Mono",
    stack: "'JetBrains Mono', ui-monospace, monospace",
    kind: "mono",
  },
];

export const FONT_SIZES = [
  { id: "s", label: "S", px: 14 },
  { id: "m", label: "M", px: 16 },
  { id: "l", label: "L", px: 18 },
  { id: "xl", label: "XL", px: 20 },
] as const;

export function fontStackFor(family: string): string {
  // family is either a bundled font id or a raw system family name typed by the user.
  const bundled = BUNDLED_FONTS.find((f) => f.id === family);
  if (bundled) return bundled.stack;
  // Treat as a system family name; quote it and add safe fallbacks.
  return `"${family}", system-ui, sans-serif`;
}
