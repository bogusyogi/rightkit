import {
  createAutoUpdateClient,
  type AutoUpdateClient,
  type AutoUpdateClientConfig,
  type CheckForUpdate,
} from "@rightkit/app-update";

export type HostAutoUpdateClientConfig = {
  check: CheckForUpdate;
  isPro: () => boolean;
  getHeaders: NonNullable<AutoUpdateClientConfig["getHeaders"]>;
  getInstallHeaders: NonNullable<AutoUpdateClientConfig["getInstallHeaders"]>;
  getProUpdateVersion?: AutoUpdateClientConfig["getProUpdateVersion"];
  canInstall?: AutoUpdateClientConfig["canInstall"];
  shouldAutoInstall?: AutoUpdateClientConfig["shouldAutoInstall"];
  deferReason?: AutoUpdateClientConfig["deferReason"];
  onStatus?: AutoUpdateClientConfig["onStatus"];
  onError?: AutoUpdateClientConfig["onError"];
};

/** The production and test entry point for the host app shared updater adapter. */
export function createHostAutoUpdateClient(
  config: HostAutoUpdateClientConfig,
): AutoUpdateClient {
  return createAutoUpdateClient({
    check: config.check,
    getProUpdateVersion: config.getProUpdateVersion,
    getHeaders: config.getHeaders,
    getInstallHeaders: config.getInstallHeaders,
    canInstall: config.canInstall,
    async isEligible(tier) {
      if (tier === null) return false;
      return tier === "patch" || config.isPro();
    },
    shouldAutoInstall: config.shouldAutoInstall,
    deferReason: config.deferReason,
    onStatus: config.onStatus,
    onError: config.onError,
  });
}
