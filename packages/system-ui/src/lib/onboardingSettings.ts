import { invoke } from "@tauri-apps/api/core";
import type { SettingsBackend, SettingsSnapshot } from "@rightkit/onboarding";

// The onboarding's own progress, kept in the hub's `hub-onboarding.json` document (a frontend-owned
// rightkit-app-settings Document). The notch's settings (docs/settings.md) hold only `onboardingDone`. The legal
// agreement is not kept here: the native receipt service records it (lib/legal.ts); the shared component only copies
// that record into `onboarding.state`, and only the one-time carry-over of an older record reads it (src-tauri/src/legal.rs).

const NAME = "onboarding";

export const onboardingSettings: SettingsBackend = {
  load: () => invoke<SettingsSnapshot>("hub_document_load", { name: NAME }),
  patch: async (patch) => {
    try {
      return await invoke<SettingsSnapshot>("hub_document_patch", { name: NAME, patch });
    } catch {
      throw new Error("Pulse could not save your setup progress.");
    }
  },
};
