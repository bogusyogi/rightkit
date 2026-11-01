import { Select, useTheme } from "@rightkit/react-shell/react";
import { PALETTES, type Palette, useThemeStore } from "@rightkit/agent-ui-core/state/themeStore";
import AnalyticsConsentCard from "../analytics/AnalyticsConsentCard";

// Dark base names per Right Suite brand.md ("Dark bases (shared): slate
// (View/Code) · charcoal (Mail) · coffee (Heard/Voice) · espresso (Scrape)").
// Light bases are all pale off-white/cream ramps across the suite.
const DARK_BASE_LABEL: Record<Palette, string> = {
  coderight: "graphite",
  viewright: "slate",
  heardright: "coffee",
  scraperight: "espresso",
  mailright: "charcoal",
};

const PALETTE_SWATCHES: { label: string; className: string }[] = [
  { label: "Background", className: "bg-bg" },
  { label: "Panel", className: "bg-panel" },
  { label: "Border", className: "bg-line" },
  { label: "Text", className: "bg-text" },
  { label: "Accent", className: "bg-accent" },
];

export default function AppearanceTab() {
  const { resolved } = useTheme();
  const paletteId = useThemeStore((state) => state.palette);
  const setPalette = useThemeStore((state) => state.setPalette);
  const baseLabel = resolved === "dark" ? DARK_BASE_LABEL[paletteId] : "off-white";
  const activePalette = PALETTES.find((p) => p.id === paletteId);

  return (
    <section className="flex max-w-[720px] flex-col gap-4 p-6">
      <div className="min-w-0">
        <h3 className="text-[16px] font-bold text-text">Appearance</h3>
        <p className="text-[13px] text-text-2">
          Keep the work surface calm while preserving the Right Suite identities.
        </p>
      </div>

      <div className="grid gap-3 rounded-lg border border-line bg-panel p-4">
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Palette
          <Select value={paletteId} options={[...PALETTES.map((palette) => ({ value: palette.id, label: palette.label }))]} onChange={(value) => setPalette(value as Palette)} label="Palette" className="h-9 rounded-lg border border-line bg-bg px-3 text-[13px] text-text outline-none" />
        </label>

        <div className="min-w-0">
          <div className="text-[13px] font-semibold text-text">Base</div>
          <div className="text-[12px] text-text-3">
            {resolved === "dark" ? `Dark ${baseLabel}` : `Light ${baseLabel}`}
          </div>
        </div>
      </div>

      <div className="rounded-lg border border-line bg-panel p-4">
        <div className="flex items-center justify-between gap-3">
          <div className="min-w-0">
            <div className="text-[13px] font-semibold text-text">Palette preview</div>
            <div className="text-[12px] text-text-3">
              {activePalette?.label ?? "Default"} · {resolved === "dark" ? "dark" : "light"}{" "}
              tokens
            </div>
          </div>
        </div>
        <div className="mt-3 grid grid-cols-5 gap-3">
          {PALETTE_SWATCHES.map((swatch) => (
            <div className="flex flex-col items-center gap-1.5 min-w-0" key={swatch.label}>
              <span className={`h-10 w-10 rounded-lg border border-line ${swatch.className}`} />
              <span className="text-[11px] text-text-3 truncate">{swatch.label}</span>
            </div>
          ))}
        </div>
      </div>

      <AnalyticsConsentCard />

      <div className="rounded-lg border border-line bg-panel p-4">
        <div className="text-[13px] font-semibold text-text">Typography</div>
        <div className="mt-2 flex flex-col gap-2">
          <div className="flex items-baseline justify-between gap-3">
            <span className="font-display text-[15px] text-text">Tanker</span>
            <span className="text-[12px] text-text-3">Display · panel titles</span>
          </div>
          <div className="flex items-baseline justify-between gap-3">
            <span className="text-[15px] text-text">General Sans</span>
            <span className="text-[12px] text-text-3">Body · UI</span>
          </div>
          <div className="flex items-baseline justify-between gap-3">
            <span className="font-mono text-[15px] text-text">JetBrains Mono</span>
            <span className="text-[12px] text-text-3">Code · paths · ids</span>
          </div>
        </div>
      </div>
    </section>
  );
}
