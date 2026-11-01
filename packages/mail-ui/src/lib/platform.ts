// Centralised OS/platform detection. Thin adapter over @rightkit/platform-ui,
// collapsing its Platform to the host app three-value OS type.

import { getPlatform as getRightkitPlatform } from "@rightkit/platform-ui";

export type OS = "macos" | "windows" | "linux";

export function getPlatform(): OS {
  const p = getRightkitPlatform();
  if (p === "windows") return "windows";
  if (p === "macos" || p === "ios") return "macos";
  return "linux";
}

export const IS_MACOS = getPlatform() === "macos";
export const IS_WINDOWS = getPlatform() === "windows";

/** Human-readable name for the OS credential store. */
export function credentialStoreName(): "macOS Keychain" | "Windows Credential Manager" | "the OS credential store" {
  const os = getPlatform();
  if (os === "macos") return "macOS Keychain";
  if (os === "windows") return "Windows Credential Manager";
  return "the OS credential store";
}

/** Human-readable name for native user verification. */
export function appLockProviderName(): "Touch ID" | "Windows Hello" | "the OS unlock prompt" {
  const os = getPlatform();
  if (os === "macos") return "Touch ID";
  if (os === "windows") return "Windows Hello";
  return "the OS unlock prompt";
}
