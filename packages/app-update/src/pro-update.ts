import { fetchReleaseStatus, isReleaseStatus, type ReleaseStatus } from './index.js';
import { releaseStatusUrl } from './suite-api.js';
import { updaterTargetForPlatform } from './target.js';
import { isNewerVersion } from './version.js';

interface ProUpdateVersionBase {
  /** The running version, or a function that reads it. */
  currentVersion: string | (() => Promise<string> | string);
}

/** Reads the release status document over https. */
export interface ProUpdateVersionRequest extends ProUpdateVersionBase {
  /** Right Suite app key; names the document on the suite API when `url` is absent. */
  app: string;
  /**
   * App or Node platform name (`macos`, `darwin`, `windows`, `win32`, `linux`), sent as the updater target.
   * `null` or a name `updaterTargetForPlatform` does not know answers `null` without a request.
   */
  platform: string | null;
  /** The release status document of an app whose API is not on the suite origin. Default `releaseStatusUrl(app)`. */
  url?: string;
  /** The fetch to use, for a webview that may not reach the API itself. */
  fetch?: typeof fetch;
}

/** Reads the release status another way, for example `native.releaseStatus()` of `@rightkit/app-update/native`. */
export interface ProUpdateVersionRead extends ProUpdateVersionBase {
  readStatus: () => Promise<ReleaseStatus> | ReleaseStatus;
}

export type ProUpdateVersionOptions = ProUpdateVersionRequest | ProUpdateVersionRead;

/**
 * The newest feature (`update` tier) version the release status names, when it is newer than the running one;
 * otherwise `null`. This is the `getProUpdateVersion` / `proUpdateVersion` nudge for an install whose own
 * manifest does not show the feature release. Advisory only, so it never throws: an unknown platform, a failed
 * or invalid read and an unreadable version all answer `null`.
 */
export async function proUpdateVersion(options: ProUpdateVersionOptions): Promise<string | null> {
  try {
    let status: ReleaseStatus;
    if ('readStatus' in options) {
      status = await options.readStatus();
      if (!isReleaseStatus(status)) return null;
    } else {
      if (options.platform === null) return null;
      const target = updaterTargetForPlatform(options.platform);
      const url = `${options.url ?? releaseStatusUrl(options.app)}?platform=${encodeURIComponent(target)}`;
      status = await fetchReleaseStatus(url, options.fetch);
    }
    if (!status.updateVersion) return null;
    const current = typeof options.currentVersion === 'function' ? await options.currentVersion() : options.currentVersion;
    return isNewerVersion(status.updateVersion, current) ? status.updateVersion : null;
  } catch {
    return null;
  }
}
