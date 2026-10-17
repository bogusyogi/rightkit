import { useEffect, useMemo, useSyncExternalStore, type ReactNode } from "react";
import { SegmentedControl, createThemeStore, useMediaQuery, useShellOptional, type ThemeChoice, type ThemeStore } from "@rightkit/react-shell/react";
import { applyBrand, brandFromAccent } from "@rightkit/theme";
import { sectionIcons } from "./icons.js";
import { SettingRow, SettingsGroup } from "./primitives.js";
import type { SettingsContext, SettingsSection } from "./types.js";

/* ---- Appearance ----------------------------------------------------------------------------- */

export interface AppearanceOptions {
  /** Theme store; defaults to the surrounding `ShellProvider`'s, else a private `rightkit.theme` store. */
  themeStore?: ThemeStore;
  /** Optional accent presets (user-chosen colour on top of the brand). The first is treated as the brand default. */
  accents?: ReadonlyArray<{ id: string; label: string; color: string }>;
  /** Extra rows (density, font size) the app adds under Appearance. */
  extra?: (ctx: SettingsContext) => ReactNode;
}

const THEME_OPTIONS = [
  { value: "system" as const, label: "System" },
  { value: "light" as const, label: "Light" },
  { value: "dark" as const, label: "Dark" },
];

function AppearanceBody({ ctx, options }: { ctx: SettingsContext; options: AppearanceOptions }) {
  const shell = useShellOptional();
  const fallback = useMemo(() => options.themeStore ?? createThemeStore({ storageKey: "rightkit.theme" }), [options.themeStore]);
  const store = options.themeStore ?? shell?.themeStore ?? fallback;
  const choice = useSyncExternalStore(store.subscribe, store.read, store.read);
  const systemDark = useMediaQuery("(prefers-color-scheme: dark)");
  const resolved = choice === "system" ? (systemDark ? "dark" : "light") : choice;
  const saved = ctx.values["appearance.theme"] as ThemeChoice | undefined;
  const accentId = ctx.values["appearance.accent"] as string | undefined;
  const accents = options.accents ?? [];

  // Settings are the source of truth across windows; the theme store mirrors them.
  useEffect(() => {
    if ((saved === "light" || saved === "dark" || saved === "system") && saved !== store.read()) store.save(saved);
  }, [saved, store]);

  const accent = accents.find((a) => a.id === accentId);
  useEffect(() => {
    if (!accent || accent.id === accents[0]?.id) return;
    return applyBrand(brandFromAccent(accent.label, accent.color), resolved);
  }, [accent, accents, resolved]);

  return (
    <>
      <SettingsGroup>
        <SettingRow id="appearance.theme" label="Theme" description="System follows your operating system, and switches when it does.">
          <SegmentedControl<ThemeChoice>
            label="Theme"
            value={choice}
            options={THEME_OPTIONS}
            onChange={(next) => {
              store.save(next);
              void ctx.set("appearance.theme", next);
            }}
          />
        </SettingRow>
        {accents.length ? (
          <SettingRow id="appearance.accent" label="Accent colour" description="Used for buttons, switches and selection.">
            <div className="rk-swatches" role="radiogroup" aria-label="Accent colour">
              {accents.map((a, i) => {
                const on = (accentId ?? accents[0].id) === a.id;
                return (
                  <button
                    key={a.id}
                    type="button"
                    role="radio"
                    aria-checked={on}
                    aria-label={a.label}
                    title={a.label}
                    className={on ? "rk-swatch is-on" : "rk-swatch"}
                    style={{ background: a.color }}
                    onClick={() => void ctx.set("appearance.accent", i === 0 ? null : a.id)}
                  />
                );
              })}
            </div>
          </SettingRow>
        ) : null}
      </SettingsGroup>
      {options.extra?.(ctx)}
    </>
  );
}

export function appearanceSection(options: AppearanceOptions = {}): SettingsSection {
  return {
    id: "appearance",
    icon: sectionIcons.appearance,
    label: "Appearance",
    group: "General",
    order: 100,
    keywords: ["theme", "dark", "light", "colour", "color", "accent"],
    entries: [
      { id: "appearance.theme", label: "Theme", keywords: ["dark", "light", "system", "mode"] },
      ...(options.accents?.length ? [{ id: "appearance.accent", label: "Accent colour", keywords: ["color", "brand"] }] : []),
    ],
    render: (ctx) => <AppearanceBody ctx={ctx} options={options} />,
  };
}
