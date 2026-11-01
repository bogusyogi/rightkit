import type { RuntimeUpdate, UpdateDownloadEvent } from "@rightkit/app-update";

/** Shape the native `update_native_check` command returns. Carries no credential. */
export interface NativeUpdateInfo {
  version: string;
  rawJson?: Record<string, unknown>;
  /** Opaque identity of the native check; the install must present it. */
  checkId: string;
}

export type NativeUpdateInstall = (
  onEvent: (event: UpdateDownloadEvent) => void,
) => Promise<void>;

/** Wrap the native update check/install as the `RuntimeUpdate` the shared client expects.
 * The manifest check and the Pro artifact download attach their Bearer headers in Rust, so
 * the signed entitlement token never reaches the webview. */
export function runtimeUpdateFromNative(
  info: NativeUpdateInfo,
  install: NativeUpdateInstall,
): RuntimeUpdate {
  return {
    version: info.version,
    rawJson: info.rawJson,
    downloadAndInstall: (onEvent) => install(onEvent ?? (() => undefined)),
  };
}

export function releaseStatusUrl(baseUrl: string, platform: string): string {
  const target = platform === "macos" ? "darwin" : platform;
  return `${baseUrl}?platform=${encodeURIComponent(target)}`;
}

