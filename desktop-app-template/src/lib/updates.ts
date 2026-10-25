import { createAutoUpdateClient } from "@rightkit/app-update";
import { check } from "@tauri-apps/plugin-updater";

// Policy (patch vs update tier, eligibility) is @rightkit/app-update; this adapter only supplies the
// host `check`. Licensed apps add `isEligible`/`getHeaders` from their license client.
export const updates = createAutoUpdateClient({
  check: async () => {
    const update = await check();
    return update ? { version: update.version, downloadAndInstall: update.downloadAndInstall.bind(update), rawJson: update.rawJson } : null;
  },
});
