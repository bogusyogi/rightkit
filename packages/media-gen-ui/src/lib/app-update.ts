import { createAutoUpdateClient, type AutoUpdateClient, type CheckForUpdate } from '@rightkit/app-update';

// RightKit update-policy client for the host app. The package has no Tauri dependency: the app injects
// the `check` function (see app-update-tauri.ts). Tauri's updater plugin still verifies the artifact
// signature against the configured pubkey (src-tauri/tauri.conf.json).
//
// Policy: release downloads are public patches, so no credential is sent with the artifact request.
// `getInstallHeaders` is set explicitly and `strictInstallHeaders` stops the manifest-check headers
// from being reused for the download (package known issue: legacy reuse can cross artifact origins).
// Auto-install stays off (the package default), so the user starts the install from Settings.
export function createAppUpdateClient(check: CheckForUpdate): AutoUpdateClient {
  return createAutoUpdateClient({
    check,
    getInstallHeaders: () => undefined,
    strictInstallHeaders: true,
  });
}
