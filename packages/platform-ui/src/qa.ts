/** QA-only platform detection. Import this entry for platform overrides. */
import { normalizePlatform, formatChord as formatChordFor, type FormatChordOptions, type Platform } from "./format.js";
import { configurePlatform as configureHostPlatform, getPlatform as getHostPlatform, loadPlatform as loadHostPlatform, isTauri, platformCapabilities as hostCapabilities, type PlatformOptions } from "./index.js";
export * from "./index.js";

let nativeWinsOverQa = false;
let nativeResolved = false;
let fallbackPlatform: string | undefined;
export function configurePlatform(options: PlatformOptions): void {
  configureHostPlatform(options);
  nativeWinsOverQa = options.nativeWinsOverQa ?? nativeWinsOverQa;
  fallbackPlatform = options.fallbackPlatform ?? fallbackPlatform;
}
export async function loadPlatform(): Promise<Platform> {
  const platform = await loadHostPlatform();
  nativeResolved = isTauri() || globalThis.__RIGHTKIT_PLATFORM__ !== undefined;
  return platform;
}
export function getPlatform(): Platform {
  const qa = globalThis.__RIGHTKIT_QA_PLATFORM__;
  const nativeHost = nativeResolved || isTauri() || globalThis.__RIGHTKIT_PLATFORM__ !== undefined;
  if (qa !== undefined && !(nativeWinsOverQa && nativeHost)) {
    const platform = normalizePlatform(qa);
    return platform === "unknown" && fallbackPlatform !== undefined ? normalizePlatform(fallbackPlatform) : platform;
  }
  return getHostPlatform();
}
export function formatChord(chord: string, platform: Platform = getPlatform(), options: FormatChordOptions = {}): string[] {
  return formatChordFor(chord, platform, options);
}
export function platformCapabilities(platform: Platform = getPlatform()) {
  return hostCapabilities(platform);
}

export interface QaPlatformBootstrapInput {
  /** Raw query string. The override is ignored unless it also contains `qa=1` (see `configureQaPlatform`). */
  query?: string;
  /** Canonical values copied explicitly from an app's build environment. */
  env?: Readonly<{
    RIGHTKIT_QA?: string | boolean;
    RIGHTKIT_QA_PLATFORM?: string;
  }>;
}

export interface QaPlatformOptions {
  /** Query parameter that carries the QA platform. Default `"qaPlatform"`. */
  qaQueryParam?: string;
  /** When false, the query parameter alone enables the QA override (no `qa=1`). Default `true`. */
  qaRequireQaFlag?: boolean;
}

declare global {
  var __RIGHTKIT_QA_PLATFORM__: string | undefined;
}

let qaConfig: { qaQueryParam: string; qaRequireQaFlag: boolean } = {
  qaQueryParam: "qaPlatform",
  qaRequireQaFlag: true,
};

/** Set QA platform options. Merges into the previous configuration; call before resolving. */
export function configureQaPlatform(options: QaPlatformOptions): void {
  qaConfig = {
    qaQueryParam: options.qaQueryParam ?? qaConfig.qaQueryParam,
    qaRequireQaFlag: options.qaRequireQaFlag ?? qaConfig.qaRequireQaFlag,
  };
}

export function resolveQaPlatform(input: QaPlatformBootstrapInput): Platform | undefined {
  if (input.query !== undefined) {
    const query = new URLSearchParams(input.query);
    const explicitPlatform = query.get(qaConfig.qaQueryParam);
    if ((!qaConfig.qaRequireQaFlag || query.get("qa") === "1") && explicitPlatform !== null) {
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
