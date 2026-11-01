import type { ReactNode } from "react";
import { resolveAccentColors, resolveTheme, useSettings } from "../settings";
import {
  ACCENT_PRESETS,
  DEFAULT_DARK,
  DEFAULT_LIGHT,
  THEMES,
  themeById,
  type AccentPreset,
} from "../themes";

export function ThemeSection() {
  const { settings, update, reset } = useSettings();
  const theme = resolveTheme(settings);
  const accents = resolveAccentColors(settings);
  const primaryId = activeAccentId(settings.primaryAccentId, accents.primary, theme.mode);
  const secondaryId = activeAccentId(settings.secondaryAccentId, accents.secondary, theme.mode);

  return (
    <div className="space-y-3">
      <ThemeGroup title="Theme" hint="Applies to shell and documents; Auto follows the OS">
        <div className="flex items-center gap-2">
          <div className="grid min-w-0 flex-1 grid-cols-[repeat(auto-fit,minmax(100px,1fr))] gap-1.5">
            <ThemeSwatch
              name="Auto"
              colors={[themeById(DEFAULT_LIGHT).tokens.ink, themeById(DEFAULT_DARK).tokens.ink]}
              selected={settings.themeId === "auto"}
              onSelect={() => update({ themeId: "auto" })}
            />
            {THEMES.map((preset) => (
              <ThemeSwatch
                key={preset.id}
                name={preset.name}
                colors={[preset.tokens.chrome, preset.tokens.ink, preset.tokens.paper]}
                selected={settings.themeId === preset.id}
                resolved={settings.themeId === "auto" && theme.id === preset.id}
                onSelect={() => update({ themeId: preset.id })}
              />
            ))}
          </div>
          <button
            type="button"
            onClick={reset}
            className="vr-press vr-focus rounded-md border border-line px-2.5 py-1.5 font-sans text-xs text-muted hover:bg-panel hover:text-paper"
          >
            Reset
          </button>
        </div>
      </ThemeGroup>
      <AccentChoices
        title="Interface accent"
        hint="Tools, links, focus, and selection"
        selectedId={primaryId}
        mode={theme.mode}
        onSelect={(primaryAccentId) => update({ primaryAccentId, customPrimary: null })}
      />
      <AccentChoices
        title="Code accent"
        hint="Syntax and secondary status"
        selectedId={secondaryId}
        mode={theme.mode}
        onSelect={(secondaryAccentId) => update({ secondaryAccentId, customSecondary: null })}
      />
    </div>
  );
}

function ThemeSwatch({
  name,
  colors,
  selected,
  resolved = false,
  onSelect,
}: {
  name: string;
  colors: string[];
  selected: boolean;
  /** While "Auto" is the selection, this marks the theme the OS actually resolved to. */
  resolved?: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={`Theme: ${name}`}
      aria-pressed={selected}
      title={resolved ? `${name} (Auto)` : name}
      onClick={onSelect}
      className={`vr-press vr-focus relative min-w-0 rounded-md border p-1.5 text-left transition-colors ${
        selected ? "border-copper ring-1 ring-copper/35" : "border-line hover:border-muted-2"
      }`}
    >
      <span
        aria-hidden
        className="mb-1 block h-10 overflow-hidden rounded-sm border border-line"
        style={{ background: colors[0] }}
      >
        <span className="block h-2.5 border-b border-white/10" />
        <span
          className="ml-3 block h-7 rounded-tl-sm px-2 py-1.5"
          style={{ background: colors[1] }}
        >
          <span
            className="mb-1 block h-0.5 w-3/4 opacity-70"
            style={{ background: colors[2] ?? colors[0] }}
          />
          <span
            className="block h-0.5 w-1/2 opacity-40"
            style={{ background: colors[2] ?? colors[0] }}
          />
        </span>
      </span>
      <span
        className={`block whitespace-normal font-sans text-chrome-xs ${selected ? "text-paper" : "text-muted"}`}
      >
        {name}
      </span>
      {resolved && (
        <span
          aria-hidden
          data-vr-theme-resolved
          className="absolute -top-1 right-1 h-2 w-2 rounded-full bg-paper ring-1 ring-line"
        />
      )}
    </button>
  );
}

function AccentChoices({
  title,
  hint,
  selectedId,
  mode,
  onSelect,
}: {
  title: string;
  hint: string;
  selectedId: string | null;
  mode: "dark" | "light";
  onSelect: (id: string) => void;
}) {
  return (
    <ThemeGroup title={title} hint={hint}>
      <div className="flex flex-wrap gap-1.5">
        {ACCENT_PRESETS.map((accent) => {
          const selected = selectedId === accent.id;
          return (
            <button
              type="button"
              key={accent.id}
              title={accent.name}
              aria-label={`${title}: ${accent.name}`}
              aria-pressed={selected}
              onClick={() => onSelect(accent.id)}
              className={`vr-press vr-focus flex w-16 flex-col items-center gap-1 rounded-md border p-1 ${
                selected ? "border-paper ring-2 ring-paper/20" : "border-line hover:border-muted-2"
              }`}
            >
              <span className="block h-6 w-full rounded-sm" style={{ background: accent[mode] }} />
              <span
                aria-hidden
                className={`w-full whitespace-normal text-center font-sans text-chrome-xs leading-tight ${selected ? "text-paper" : "text-muted"}`}
              >
                {accent.name}
              </span>
            </button>
          );
        })}
      </div>
    </ThemeGroup>
  );
}

function ThemeLabel({ children }: { children: string }) {
  return (
    <div className="mb-1.5 font-sans text-chrome-xs font-semibold uppercase tracking-[0.18em] text-muted-2">
      {children}
    </div>
  );
}

function ThemeGroup({
  title,
  hint,
  children,
}: {
  title: string;
  hint: string;
  children: ReactNode;
}) {
  return (
    <section
      data-theme-row={title}
      className="grid grid-cols-[8.75rem_minmax(0,1fr)] items-center gap-3 rounded-lg border border-line bg-panel/35 p-2.5"
    >
      <div className="min-w-0">
        <ThemeLabel>{title}</ThemeLabel>
        <span className="block text-chrome-xs leading-tight text-muted-2">{hint}</span>
      </div>
      {children}
    </section>
  );
}

function activeAccentId(
  selectedId: string | null,
  color: string,
  mode: "dark" | "light",
): string | null {
  if (selectedId) return selectedId;
  return ACCENT_PRESETS.find((accent: AccentPreset) => accent[mode] === color)?.id ?? null;
}
