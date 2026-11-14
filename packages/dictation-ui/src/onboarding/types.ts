export type OS = "macos" | "ios" | "windows" | "linux";
export type Step = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
export type PermissionState = "ready" | "needed" | "waiting" | "denied" | "restricted";
export type MicCheckState = "idle" | "listening" | "verified" | "no_signal" | "unavailable";
export type ShortcutKind = "toggle" | "send" | "cancel";
