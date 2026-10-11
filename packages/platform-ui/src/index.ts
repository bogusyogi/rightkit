import { formatChord as formatChordFor, normalizePlatform, type FormatChordOptions, type Platform } from "./format.js";

export { normalizePlatform, type FormatChordOptions, type Platform } from "./format.js";

export interface QaPlatformBootstrapInput {
  /** Raw query string. The override is ignored unless it also contains `qa=1`. */
  query?: string;
  /** Canonical values copied explicitly from an app's build environment. */
  env?: Readonly<{
    RIGHTKIT_QA?: string | boolean;
    RIGHTKIT_QA_PLATFORM?: string;
  }>;
}

declare global {
  // Browser QA must opt into a deterministic platform; production uses Tauri.
  var __RIGHTKIT_QA_PLATFORM__: string | undefined;
}

export function resolveQaPlatform(input: QaPlatformBootstrapInput): Platform | undefined {
  if (input.query !== undefined) {
    const query = new URLSearchParams(input.query);
    const explicitPlatform = query.get("qaPlatform");
    if (query.get("qa") === "1" && explicitPlatform !== null) {
      return normalizePlatform(explicitPlatform);
    }
  }

  const enabled = input.env?.RIGHTKIT_QA;
  const explicitPlatform = input.env?.RIGHTKIT_QA_PLATFORM;
  if ((enabled === true || enabled === "1") && typeof explicitPlatform === "string") {
    return normalizePlatform(explicitPlatform);
  }
  return undefined;
}

export function bootstrapQaPlatform(input: QaPlatformBootstrapInput): Platform | undefined {
  const platform = resolveQaPlatform(input);
  if (platform === undefined) {
    delete globalThis.__RIGHTKIT_QA_PLATFORM__;
    return undefined;
  }
  globalThis.__RIGHTKIT_QA_PLATFORM__ = platform;
  return platform;
}

/**
 * The native platform from `@tauri-apps/plugin-os`, once loaded. `plugin-os` is loaded with a dynamic
 * import, so apps that never call native detection never pull it in, and an app without the plugin
 * installed falls back to the browser's report instead of failing.
 */
let nativePlatform: Platform | undefined;
let nativeLoad: Promise<Platform> | undefined;

function browserPlatform(): Platform {
  const nav = (globalThis as { navigator?: { platform?: string; userAgent?: string; userAgentData?: { platform?: string } } }).navigator;
  const raw = `${nav?.userAgentData?.platform ?? ""} ${nav?.platform ?? ""} ${nav?.userAgent ?? ""}`.toLowerCase();
  if (/iphone|ipad|ipod/.test(raw)) return normalizePlatform("ios");
  if (/mac/.test(raw)) return normalizePlatform("macos");
  if (/win/.test(raw)) return normalizePlatform("windows");
  if (/android/.test(raw)) return normalizePlatform("android");
  if (/linux/.test(raw)) return normalizePlatform("linux");
  return "unknown";
}

/**
 * Resolve the native platform (loads `@tauri-apps/plugin-os` on first call). Outside Tauri, or when
 * the plugin is absent, the result is the browser's own platform report. Resolves once and caches.
 */
export function loadPlatform(): Promise<Platform> {
  if (nativePlatform !== undefined) return Promise.resolve(nativePlatform);
  nativeLoad ??= import("@tauri-apps/plugin-os")
    .then((os) => {
      // Throws outside Tauri (no injected internals); the catch below then takes the browser report.
      nativePlatform = normalizePlatform(os.platform());
    })
    .catch(() => {
      // Outside Tauri (browser dev, previews) or plugin not installed.
      nativePlatform = browserPlatform();
    })
    .then(() => nativePlatform as Platform);
  return nativeLoad;
}

/**
 * Synchronous platform. A QA override wins; then the native result once `loadPlatform` has resolved.
 * Before that it returns the browser's report (correct for every supported desktop webview) and
 * starts the native load in the background.
 */
export function getPlatform(): Platform {
  if (globalThis.__RIGHTKIT_QA_PLATFORM__ !== undefined) {
    return normalizePlatform(globalThis.__RIGHTKIT_QA_PLATFORM__);
  }
  if (nativePlatform !== undefined) return nativePlatform;
  void loadPlatform();
  return browserPlatform();
}

/** Display tokens for a canonical chord. `platform` defaults to the detected platform. */
export function formatChord(chord: string, platform: Platform = getPlatform(), options: FormatChordOptions = {}): string[] {
  return formatChordFor(chord, platform, options);
}

export function platformCapabilities(platform: Platform = getPlatform()) {
  return {
    desktopWindowControls: platform === "macos" || platform === "windows" || platform === "linux",
    nativeTrafficLights: platform === "macos",
    customCaptionButtons: platform === "windows" || platform === "linux",
    touchFirst: platform === "ios",
  } as const;
}

export const DEFAULT_WINDOW_CONTROL_LABELS = {
  minimize: "Minimize window",
  maximize: "Maximize or restore window",
  close: "Close window",
  enterFullscreen: "Enter fullscreen",
  exitFullscreen: "Exit fullscreen",
} as const;

export type WindowControlLabels = {
  [Key in keyof typeof DEFAULT_WINDOW_CONTROL_LABELS]: string;
};

/** @deprecated Prefer the explicit default name and override helper. */
export const WINDOW_CONTROL_LABELS = DEFAULT_WINDOW_CONTROL_LABELS;

export function createWindowControlLabels(
  overrides: Partial<WindowControlLabels> = {},
): WindowControlLabels {
  return { ...DEFAULT_WINDOW_CONTROL_LABELS, ...overrides };
}

export interface WindowControlAdapter {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
}

export interface FullscreenControlAdapter {
  setFullscreen(value: boolean): Promise<void>;
}

export function createWindowControls(adapter: WindowControlAdapter) {
  return {
    minimize: () => adapter.minimize(),
    toggleMaximize: () => adapter.toggleMaximize(),
    close: () => adapter.close(),
  };
}

export function createFullscreenControls(adapter: FullscreenControlAdapter) {
  return {
    enterFullscreen: () => adapter.setFullscreen(true),
    exitFullscreen: () => adapter.setFullscreen(false),
  };
}
