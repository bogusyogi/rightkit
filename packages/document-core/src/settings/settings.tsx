import {
  applyThemeAttribute,
  applyTokens,
  isHex,
  mix,
  onPrefersDarkChange,
  parseHex,
  prefersDark,
  presetTokens,
} from "@rightkit/theme";
import { toast } from "@rightkit/platform-ui/headless";
import { useSettings as useRkSettings, type SettingsValues } from "@rightkit/settings-ui";
import { createContext, useCallback, useContext, useEffect, useMemo } from "react";
import { SETTINGS_DOCUMENT_KEY, loadSettingsDocument, saveSettingsDocument } from "../state/tauriBridge";
import {
  ACCENT_PRESETS,
  DEFAULT_DARK,
  DEFAULT_LIGHT,
  THEMES,
  accentPreset,
  themeById,
  tokenPreset,
  type ThemePreset,
} from "./themes";
import { fontStackFor } from "./fonts";
import { createSettingsBackend } from "./settingsBackend";

export interface Settings {
  /** A preset id, or "auto" to follow the system. */
  themeId: string;
  /** Finite palette choices. Null follows a legacy custom color or the selected theme preset. */
  primaryAccentId: string | null;
  secondaryAccentId: string | null;
  customPrimary: string | null;
  customSecondary: string | null;
  /** Bundled font id or a raw system family name. */
  fontFamily: string;
  fontSizePx: number;
  sourceFontSizePx: number;
  lineHeight: number;
  /** Prose measure for the Markdown reading column (px). Wide tables and code fences may still
   *  break out of it; other formats keep their own surface sizing. */
  readingWidthPx: number;
  /** Full available width by default; a centred reading column is opt-in. */
  readingWidthMode: "full" | "column";
  contrast: "normal" | "high";
  /** In-app motion override: "system" follows prefers-reduced-motion; "reduce"/"full" force it.
   *  Applied as data-motion on the root (see applySettings). */
  motion: "system" | "reduce" | "full";
  /** Soft-wrap long lines in the text editor (Notepad's Word Wrap). */
  wordWrap: boolean;
  /** Use the platform spellchecker on document-body editing surfaces. */
  spellcheck: boolean;
  /** Reformat md/mdx/html source on save (opt-in — off by default to avoid surprise diffs). */
  formatOnSave: boolean;
  /** Render sanitized raw HTML in Markdown previews and exports. Source stays raw. */
  markdownSafeHtml: boolean;
  /** Save file-backed edits after a short idle delay. Untitled drafts stay local until first Save. */
  autoSave: boolean;
  /** Restore file-backed tabs from the last desktop session. Untitled scratch buffers are always
   *  locally draft-backed so Notepad-style notes survive relaunch. */
  restoreSession: boolean;
  /** Persist local renderer & native diagnostic logs (loads/errors) on device; this toggle controls
   *  whether diagnostics are written at all. Read by diagnostics.ts & native log gate. */
  diagnostics: boolean;
  /** Check for updates automatically while the app is running. Installs still require user action. */
  autoCheckUpdates: boolean;
  /** Right Suite desktop legal-gate acceptance receipt (src/app/AppRoot.tsx + src/state/legal.ts).
   *  Empty string = never accepted. All four fields are set together by the gate's onAgree handler
   *  and read together by legalAcceptanceCurrent() — never partially. */
  legalAcceptanceVersion: string;
  /** Canonical UTC instant (`new Date().toISOString()`), e.g. "2026-07-17T09:41:12.345Z". */
  legalAcceptedAt: string;
  legalEligibilityBasis: "individual" | "enterprise" | null;
  legalMaterialSha256: string;
}

const DEFAULTS: Settings = {
  // New installs follow the system: Paper in light mode, Black & Grey in dark.
  themeId: "auto",
  primaryAccentId: null,
  secondaryAccentId: null,
  customPrimary: null,
  customSecondary: null,
  fontFamily: "sentient",
  fontSizePx: 16,
  sourceFontSizePx: 12,
  lineHeight: 1.7,
  readingWidthPx: 700,
  readingWidthMode: "full",
  contrast: "normal",
  motion: "system",
  wordWrap: true,
  spellcheck: true,
  formatOnSave: false,
  markdownSafeHtml: false,
  autoSave: false,
  restoreSession: true,
  diagnostics: true,
  autoCheckUpdates: false,
  legalAcceptanceVersion: "",
  legalAcceptedAt: "",
  legalEligibilityBasis: null,
  legalMaterialSha256: "",
};

/** The boot cache in localStorage: a synchronous copy for first paint, written only after the native
 *  document commits and never authoritative. */
const STORE_KEY = SETTINGS_DOCUMENT_KEY;

function parseSettingsDocument(raw: string): Settings {
  const parsed = JSON.parse(raw) as Partial<Settings>;
  // Unknown or removed ids land on a shipped theme (the default for light) rather than resolving
  // to a missing preset.
  parsed.themeId = parsed.themeId === "auto" ? "auto" : (knownThemeId(parsed.themeId) ?? DEFAULT_LIGHT);
  const merged = { ...DEFAULTS, ...parsed };
  // Clamp the persisted measure so a corrupt or hand-edited payload can't lay out an absurd column.
  merged.readingWidthPx = Math.min(
    1100,
    Math.max(480, Number(merged.readingWidthPx) || DEFAULTS.readingWidthPx),
  );
  merged.readingWidthMode = merged.readingWidthMode === "column" ? "column" : "full";
  merged.motion = merged.motion === "reduce" || merged.motion === "full" ? merged.motion : "system";
  return merged;
}

/** Removed preset ids → their replacement in the four-theme set. */
const REMOVED_THEME_IDS: Record<string, string> = {
  "paper-ink": "viewright-paper",
  "daylight-slate": "viewright-paper",
  "parchment-sienna": "viewright-paper",
  "viewright-night": "black-grey",
  "slate-amber": "black-grey",
};

/** Resolve a persisted id to a shipped theme id, following the removed-id remaps. Null if unknown. */
function knownThemeId(id: unknown): string | null {
  if (typeof id !== "string") return null;
  const mapped = REMOVED_THEME_IDS[id] ?? id;
  return THEMES.some((t) => t.id === mapped) ? mapped : null;
}

export function resolveTheme(settings: Settings): ThemePreset {
  if (settings.themeId === "auto") return themeById(prefersDark() ? DEFAULT_DARK : DEFAULT_LIGHT);
  return themeById(settings.themeId);
}

/** One theme choice applies to shell and body together; kept as a separate export for call sites. */
export function resolveShellTheme(settings: Settings): ThemePreset {
  return resolveTheme(settings);
}

export function resolveAccentColors(settings: Settings): { primary: string; secondary: string } {
  const theme = resolveTheme(settings);
  const pick = (
    id: string | null,
    variable: "--copper" | "--accent-2",
    custom: string | null,
    fallback: string,
  ): string =>
    id != null && ACCENT_PRESETS.some((item) => item.id === id)
      ? (presetTokens(accentPreset(theme, variable), id)[variable] ?? fallback)
      : (custom ?? fallback);
  return {
    primary: pick(settings.primaryAccentId, "--copper", settings.customPrimary, theme.tokens.primary),
    secondary: pick(
      settings.secondaryAccentId,
      "--accent-2",
      settings.customSecondary,
      theme.tokens.secondary,
    ),
  };
}

/** Hover shade for an overridden accent: toward white in dark mode, toward black in light mode.
 *  Mixing replaces the old flat +/-10% channel shift (t chosen to land within a few levels of it
 *  for typical accents); non-hex values pass through unchanged as before. */
function shiftAccent(color: string, dark: boolean): string {
  if (!isHex(color)) return color;
  return dark ? mix(color, "#FFFFFF", 0.25) : mix(color, "#000000", 0.17);
}

function rgbChannels(color: string): string {
  if (isHex(color)) {
    const { r, g, b } = parseHex(color);
    return `${r} ${g} ${b}`;
  }

  const rgb = /^rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*[0-9.]+)?\)$/i.exec(String(color).trim());
  if (rgb) return `${Number(rgb[1])} ${Number(rgb[2])} ${Number(rgb[3])}`;

  return "0 0 0";
}

/** Variables that also get a space-separated `-rgb` companion (`rgb(var(--ink-rgb) / 0.5)`).
 *  The published theme package has no channel helper (exception m2-theme-rgb-channels). */
const RGB_VARS = [
  "--ink",
  "--reader-surface",
  "--reader-text",
  "--panel",
  "--panel-2",
  "--paper",
  "--muted",
  "--muted-2",
  "--copper",
  "--copper-hi",
  "--accent-2",
  "--danger",
  "--danger-hi",
  "--chrome",
  "--chrome-2",
  "--chrome-paper",
  "--chrome-muted",
  "--chrome-muted-2",
  "--chrome-hover",
] as const;

// T2-2: module-level cache of the last-applied CSS custom props. applySettings runs on every settings
// change (theme flip, zoom tick, slider drag) and used to unconditionally rewrite ~50 root.style
// properties even when most of them were unchanged (e.g. a font-size-only zoom tick still rewrote every
// theme color var). Diffing against the last-applied value skips the no-op writes.
let lastAppliedVars: Record<string, string> | null = null;

function applySettings(settings: Settings) {
  const theme = resolveTheme(settings);
  const t = theme.tokens;
  const root = document.documentElement;

  const { primary, secondary } = resolveAccentColors(settings);
  const hasPrimaryOverride = settings.primaryAccentId != null || settings.customPrimary != null;
  const primaryHi = hasPrimaryOverride ? shiftAccent(primary, theme.mode === "dark") : t.primaryHi;

  const colors: Record<string, string> = {
    ...presetTokens(tokenPreset(theme)),
    "--copper": primary,
    "--copper-hi": primaryHi,
    "--accent-2": secondary,
  };
  const vars: Record<string, string> = {
    ...colors,
    ...Object.fromEntries(RGB_VARS.map((name) => [`${name}-rgb`, rgbChannels(colors[name])])),
    "--doc-font": fontStackFor(settings.fontFamily),
    "--doc-size": `${settings.fontSizePx}px`,
    "--source-size": `${settings.sourceFontSizePx}px`,
    "--doc-leading": String(settings.lineHeight),
    "--doc-measure":
      settings.readingWidthMode === "column" ? `${settings.readingWidthPx}px` : "100cqi",
    "--doc-fg":
      settings.contrast === "high" ? t.paper : `color-mix(in srgb, ${t.paper} 90%, transparent)`,
  };
  const previous = lastAppliedVars;
  applyTokens(
    Object.fromEntries(Object.entries(vars).filter(([k, v]) => previous?.[k] !== v)),
    root,
  );
  lastAppliedVars = vars;
  applyThemeAttribute(theme.mode, root);
  root.setAttribute("data-motion", settings.motion);
  // Remember the ink color so index.html can paint it before the bundle loads (no white flash).
  // Boot cache only (m10-boot-cache): never read back as a setting.
  if (previous?.["--ink"] !== vars["--ink"]) {
    try {
      localStorage.setItem("vr.lastInk", t.ink);
    } catch {
      /* ignore */
    }
  }
}

/** Reading the boot cache never throws: storage can be blocked or hold a payload we cannot parse. */
function readBootCache(): string | null {
  try {
    return localStorage.getItem(STORE_KEY);
  } catch {
    return null;
  }
}

function writeBootCache(values: SettingsValues): void {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(values));
  } catch {
    /* ignore */
  }
}

function parseSettings(raw: string): SettingsValues {
  return { ...parseSettingsDocument(raw) };
}

interface Ctx {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  /** Like `update`, but commits to the native store first and rejects, leaving state untouched, unless the committed document holds the change. */
  updateAsync: (patch: Partial<Settings>) => Promise<void>;
  reset: () => void;
}

/** A theme change clears the per-slot overrides so the new theme shows as chosen. */
function effectiveChange(change: Partial<Settings>): Partial<Settings> {
  return "themeId" in change
    ? {
        primaryAccentId: null,
        secondaryAccentId: null,
        customPrimary: null,
        customSecondary: null,
        ...change,
      }
    : change;
}

const SettingsContext = createContext<Ctx | null>(null);

export function SettingsProvider({ children }: { children: React.ReactNode }) {
  // First paint comes from the boot cache; the native document replaces it as soon as it loads.
  const { backend, initial, commitDurably } = useMemo(() => {
    // The store reloads (one read, no write) when its backend reports a change, which is how a
    // durable commit made outside the store's own optimistic `patch` reaches its state.
    const changeHandlers = new Set<(keys: string[]) => void>();
    let lastCommitted: string | null = null;
    let values: SettingsValues | undefined;
    const cached = readBootCache();
    if (cached) {
      try {
        values = parseSettings(cached);
      } catch {
        /* unreadable cache: start from defaults; the native document is read next */
      }
    }
    const inner = createSettingsBackend(
        {
          read: async () => {
            const { value, revision } = await loadSettingsDocument();
            return { raw: value, revision };
          },
          write: async (raw, options) => {
            const committed = await saveSettingsDocument(raw, options);
            lastCommitted = committed ? committed.value : null;
            return committed ? { raw: committed.value, revision: committed.revision } : null;
          },
          parse: parseSettings,
          readCache: readBootCache,
          writeCache: writeBootCache,
        },
        values,
      );
    const backend: typeof inner = {
      ...inner,
      subscribe: async (handler) => {
        changeHandlers.add(handler);
        return () => {
          changeHandlers.delete(handler);
        };
      },
    };
    /** One durable write that is not optimistic: resolves only when the native commit holds every
     *  key, and moves the store's state only after that. */
    const commitDurably = async (change: Partial<Settings>) => {
      const effective = effectiveChange(change);
      lastCommitted = null;
      await inner.patch(effective as SettingsValues);
      if (lastCommitted !== null) {
        const stored = JSON.parse(lastCommitted) as Record<string, unknown>;
        for (const [key, wanted] of Object.entries(effective)) {
          const durable = stored[key];
          const same =
            wanted === null || wanted === undefined
              ? durable === undefined || durable === null
              : JSON.stringify(durable) === JSON.stringify(wanted);
          if (!same) throw new Error("Your settings could not be saved.");
        }
      }
      for (const handler of [...changeHandlers]) handler(Object.keys(effective));
    };
    return {
      initial: values ? { values, revision: 0 } : undefined,
      backend,
      commitDurably,
    };
  }, []);
  const store = useRkSettings(backend, { initial });
  const { values, patch, error, clearError } = store;
  const settings = useMemo(() => ({ ...DEFAULTS, ...values }) as Settings, [values]);

  useEffect(() => {
    applySettings(settings);
  }, [settings]);

  // A failed native read or commit is shown to the user; the store has already rolled the change back.
  useEffect(() => {
    if (!error) return;
    toast(error, "error");
    clearError();
  }, [error, clearError]);

  // Re-apply when the OS theme flips and we're on "auto".
  useEffect(() => {
    if (settings.themeId !== "auto") return;
    return onPrefersDarkChange(() => applySettings(settings));
  }, [settings]);

  // The store's own `patch` is optimistic and resolves even when its write fails, and the first-run
  // gate derives from that state, so the durable path commits once through the backend and lets
  // the store reload (no second write). A failed commit rejects with state untouched and no store
  // toast: the caller owns the single error surface.
  const updateAsync = commitDurably;
  const update = useCallback(
    (change: Partial<Settings>) => {
      // Ordinary updates keep the shared store's own behaviour: optimistic state, rollback and the
      // error toast on a failed write.
      void patch(effectiveChange(change));
    },
    [patch],
  );
  const reset = useCallback(() => {
    // Reset restores preferences only; the legal acceptance receipt is a record, not a preference,
    // so resetting must not send the user back through the first-run agreement gate.
    const {
      legalAcceptanceVersion: _version,
      legalAcceptedAt: _at,
      legalEligibilityBasis: _basis,
      legalMaterialSha256: _sha,
      ...preferences
    } = DEFAULTS;
    void patch(preferences);
  }, [patch]);

  const value = useMemo<Ctx>(
    () => ({ settings, update, updateAsync, reset }),
    [settings, update, updateAsync, reset],
  );
  return <SettingsContext.Provider value={value}>{children}</SettingsContext.Provider>;
}

export function useSettings(): Ctx {
  const ctx = useContext(SettingsContext);
  if (!ctx) throw new Error("useSettings must be used within SettingsProvider");
  return ctx;
}
