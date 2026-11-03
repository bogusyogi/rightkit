/** Tauri updater target key of the platform: the manifest names `darwin`, while apps say `macos`. */
export type UpdaterTarget = 'darwin' | 'windows' | 'linux';

/**
 * Maps an app or Node platform name to the updater target key (`darwin`, `windows`, `linux`). `macos` and
 * `darwin` both map to `darwin`; `win32` and `windows` to `windows`. Any other name throws, so a typo cannot
 * reach the manifest request.
 */
export function updaterTargetForPlatform(platform: string): UpdaterTarget {
  switch (platform) {
    case 'macos':
    case 'darwin':
      return 'darwin';
    case 'windows':
    case 'win32':
      return 'windows';
    case 'linux':
      return 'linux';
    default:
      throw new TypeError(`unknown update platform: ${String(platform)}`);
  }
}
