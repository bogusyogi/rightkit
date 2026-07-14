import { platform as tauriPlatform } from "@tauri-apps/plugin-os";

export type Platform = "macos" | "ios" | "windows" | "linux";

declare global {
  // Browser QA must opt into a deterministic platform; production uses Tauri.
  var __RIGHTKIT_QA_PLATFORM__: string | undefined;
}

export function normalizePlatform(value: string): Platform {
  if (value === "macos" || value === "ios" || value === "windows" || value === "linux") {
    return value;
  }
  return "linux";
}

let cachedPlatform: Platform | undefined;

export function getPlatform(): Platform {
  if (globalThis.__RIGHTKIT_QA_PLATFORM__ !== undefined) {
    return normalizePlatform(globalThis.__RIGHTKIT_QA_PLATFORM__);
  }
  if (cachedPlatform) return cachedPlatform;
  try {
    cachedPlatform = normalizePlatform(tauriPlatform());
  } catch {
    cachedPlatform = normalizePlatform(globalThis.__RIGHTKIT_QA_PLATFORM__ ?? "linux");
  }
  return cachedPlatform;
}

export function usePlatform(): Platform {
  return getPlatform();
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

export function formatChord(chord: string, platform: Platform = getPlatform()): string[] {
  const keys = platform === "macos" ? MAC_KEYS : platform === "ios" ? IOS_KEYS : WINDOWS_KEYS;
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

export const WINDOW_CONTROL_LABELS = {
  minimize: "Minimize window",
  maximize: "Maximize or restore window",
  close: "Close window",
  enterFullscreen: "Enter fullscreen",
  exitFullscreen: "Exit fullscreen",
} as const;

export interface WindowControlAdapter {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  setFullscreen(value: boolean): Promise<void>;
}

export function createWindowControls(adapter: WindowControlAdapter) {
  return {
    minimize: () => adapter.minimize(),
    toggleMaximize: () => adapter.toggleMaximize(),
    close: () => adapter.close(),
    enterFullscreen: () => adapter.setFullscreen(true),
    exitFullscreen: () => adapter.setFullscreen(false),
  };
}
