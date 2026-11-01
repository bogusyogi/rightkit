// Platform authority is @rightkit/platform-ui: native detection through @tauri-apps/plugin-os once `loadPlatform()` has
// resolved (main.tsx awaits it before the first render). There is no query-string override in the app: UI behaviour is
// proven on each real OS by the journeys, never by pretending to be another platform.
import { getPlatform, type Platform } from "@rightkit/platform-ui";

export function detectPlatform(): Platform {
  return getPlatform();
}
