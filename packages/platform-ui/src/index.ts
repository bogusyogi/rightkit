import { formatChord as formatChordFor, normalizePlatform, type FormatChordOptions, type Platform } from "./format.js";

export { normalizePlatform, platformInitializationScript, type FormatChordOptions, type Platform } from "./format.js";

export interface QaPlatformBootstrapInput {
  /** Raw query string. The override is ignored unless it also contains `qa=1` (see `configurePlatform`). */
  query?: string;
  /** Canonical values copied explicitly from an app's build environment. */
  env?: Readonly<{
    RIGHTKIT_QA?: string | boolean;
    RIGHTKIT_QA_PLATFORM?: string;
  }>;
}

/**
 * Options for `configurePlatform`. Every field has the default shown; an unconfigured module behaves as
 * 0.2.7 did. Calls merge into the previous configuration.
 */
export interface PlatformOptions {
  /** Query parameter that carries the QA platform. Default `"qaPlatform"`. */
  qaQueryParam?: string;
  /** When false, the query parameter alone enables the QA override (no `qa=1`). Default `true`. */
  qaRequireQaFlag?: boolean;
  /** When true, a native host (Tauri) ignores the QA override and reports the native platform. Default `false`. */
  nativeWinsOverQa?: boolean;
  /**
   * Platform used only when the resolved platform would be `unknown` (an unsupported native name, or no
   * native and no browser match). Normalised like any platform name; an unsupported name leaves `unknown`.
   */
  fallbackPlatform?: string;
}

declare global {
  // Browser QA must opt into a deterministic platform; production uses Tauri.
  var __RIGHTKIT_QA_PLATFORM__: string | undefined;
  // Set by the host before any script runs (see `platformInitializationScript`). Read synchronously.
  var __RIGHTKIT_PLATFORM__: string | undefined;
}

let config: { qaQueryParam: string; qaRequireQaFlag: boolean; nativeWinsOverQa: boolean; fallbackPlatform: string | undefined } = {
  qaQueryParam: "qaPlatform",
  qaRequireQaFlag: true,
  nativeWinsOverQa: false,
  fallbackPlatform: undefined,
};

/** Set platform options. Merges into the previous configuration; call it before the first platform read. */
export function configurePlatform(options: PlatformOptions): void {
  config = {
    qaQueryParam: options.qaQueryParam ?? config.qaQueryParam,
    qaRequireQaFlag: options.qaRequireQaFlag ?? config.qaRequireQaFlag,
    nativeWinsOverQa: options.nativeWinsOverQa ?? config.nativeWinsOverQa,
    fallbackPlatform: options.fallbackPlatform ?? config.fallbackPlatform,
  };
}

export function resolveQaPlatform(input: QaPlatformBootstrapInput): Platform | undefined {
  if (input.query !== undefined) {
    const query = new URLSearchParams(input.query);
    const explicitPlatform = query.get(config.qaQueryParam);
    if ((!config.qaRequireQaFlag || query.get("qa") === "1") && explicitPlatform !== null) {
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
/** True once `plugin-os` answered (inside Tauri). Used by the native-wins rule. */
let pluginResolved = false;

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

/** The host-injected platform, normalised, or `undefined` when the host set none. */
function injectedPlatform(): Platform | undefined {
  const raw: unknown = globalThis.__RIGHTKIT_PLATFORM__;
  return typeof raw === "string" && raw !== "" ? normalizePlatform(raw) : undefined;
}

/** A native host: the host injected a platform, plugin-os answered, or the Tauri internals exist. */
function inNativeHost(): boolean {
  return pluginResolved || injectedPlatform() !== undefined || "__TAURI_INTERNALS__" in globalThis;
}

function withFallback(platform: Platform): Platform {
  if (platform !== "unknown" || config.fallbackPlatform === undefined) return platform;
  return normalizePlatform(config.fallbackPlatform);
}

/**
 * Resolve the native platform (loads `@tauri-apps/plugin-os` on first call). Outside Tauri, or when
 * the plugin is absent, the result is the host-injected platform if any, else the browser's own report.
 * Resolves once and caches. If a host-injected value disagrees with the native one, the native value wins
 * and a warning is logged.
 */
export function loadPlatform(): Promise<Platform> {
  if (nativePlatform !== undefined) return Promise.resolve(withFallback(nativePlatform));
  nativeLoad ??= import("@tauri-apps/plugin-os")
    .then((os) => {
      const native = normalizePlatform(os.platform());
      pluginResolved = true;
      const injected = injectedPlatform();
      if (injected !== undefined && injected !== native) {
        console.warn(`[rightkit] injected platform "${injected}" disagrees with native "${native}"; using native.`);
      }
      nativePlatform = native;
    })
    .catch(() => {
      // Outside Tauri (browser dev, previews) or plugin not installed. A host-injected value still applies.
      nativePlatform = injectedPlatform() ?? browserPlatform();
    })
    .then(() => withFallback(nativePlatform as Platform));
  return nativeLoad;
}

/**
 * Synchronous platform. A QA override wins (unless `nativeWinsOverQa` and the app runs in a native host);
 * then the native result once `loadPlatform` has resolved; before that the host-injected value
 * (`window.__RIGHTKIT_PLATFORM__`, see `platformInitializationScript`) when present, else the browser's
 * report (correct for every supported desktop webview). Starts the native load in the background.
 */
export function getPlatform(): Platform {
  return withFallback(resolvePlatform());
}

function resolvePlatform(): Platform {
  const qa = globalThis.__RIGHTKIT_QA_PLATFORM__;
  if (qa !== undefined && !(config.nativeWinsOverQa && inNativeHost())) {
    return normalizePlatform(qa);
  }
  if (nativePlatform !== undefined) return nativePlatform;
  void loadPlatform();
  const injected = injectedPlatform();
  if (injected !== undefined) return injected;
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
