import { platform as tauriPlatform } from "@tauri-apps/plugin-os";

export type Platform = "macos" | "ios" | "windows" | "linux" | "unknown";

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

export function normalizePlatform(value: string): Platform {
  if (value === "Win32" || value === "win32") return "windows";
  if (value === "macos" || value === "ios" || value === "windows" || value === "linux") {
    return value;
  }
  return "unknown";
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

let cachedPlatform: Platform | undefined;

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

export function getPlatform(): Platform {
  if (globalThis.__RIGHTKIT_QA_PLATFORM__ !== undefined) {
    return normalizePlatform(globalThis.__RIGHTKIT_QA_PLATFORM__);
  }
  if (cachedPlatform) return cachedPlatform;
  try {
    cachedPlatform = normalizePlatform(tauriPlatform());
  } catch {
    // Outside Tauri (browser dev, previews) fall back to the browser's own platform report.
    cachedPlatform = browserPlatform();
  }
  return cachedPlatform;
}

const MAC_KEYS: Readonly<Record<string, string>> = {
  mod: "⌘",
  ctrl: "⌃",
  alt: "⌥",
  shift: "⇧",
  fn: "fn",
  win: "⌘",
  space: "Space",
  enter: "↩",
  esc: "Esc",
};

const WINDOWS_KEYS: Readonly<Record<string, string>> = {
  mod: "Ctrl",
  ctrl: "Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  ralt: "Right Alt",
  shift: "Shift",
  fn: "Fn",
  win: "Win",
  space: "Space",
  enter: "Enter",
  esc: "Esc",
};

const IOS_KEYS: Readonly<Record<string, string>> = {
  mod: "Touch",
  ctrl: "Touch",
  alt: "Touch",
  shift: "Touch",
  fn: "Touch",
  win: "Touch",
  space: "Record",
  enter: "Send",
  esc: "Cancel",
  record: "Record",
  send: "Send",
  cancel: "Cancel",
};

const GENERIC_KEYS: Readonly<Record<string, string>> = {
  mod: "Mod",
  ctrl: "Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  ralt: "Right Alt",
  shift: "Shift",
  fn: "Fn",
  win: "System",
  space: "Space",
  enter: "Enter",
  esc: "Esc",
};

export function formatChord(chord: string, platform: Platform = getPlatform()): string[] {
  const keys =
    platform === "macos"
      ? MAC_KEYS
      : platform === "ios"
        ? IOS_KEYS
        : platform === "windows" || platform === "linux"
          ? WINDOWS_KEYS
          : GENERIC_KEYS;
  return chord
    .split("+")
    .map((token) => token.trim().toLowerCase())
    .filter(Boolean)
    .map((token) =>
      keys[token] ??
      (token.length === 1 ? token.toUpperCase() : token.replace(/^\w/, (letter) => letter.toUpperCase())),
    );
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
