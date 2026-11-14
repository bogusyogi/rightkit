import { getPlatform } from "@rightkit/platform-ui";
import type { OS } from "./types.js";

/** The platform the setup draws for; the shared `"unknown"` (Android, unrecognised agents) reads as `"linux"`. */
export function getOnboardingPlatform(): OS {
  const platform = getPlatform();
  return platform === "unknown" ? "linux" : platform;
}

export function usePlatform(): OS {
  return getOnboardingPlatform();
}
