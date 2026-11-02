// Platform detection for window chrome (traffic lights vs. Windows buttons).
// Delegates to @rightkit/platform-ui (the Right Suite platform authority).

import * as platformUi from '@rightkit/platform-ui';

export type OS = 'macos' | 'windows' | 'linux';

// The QA entry (browser QA's `?qa=1&qaPlatform=mac|win|linux` override) is loaded only in dev builds,
// so a release bundle carries no QA code.
const platform: typeof platformUi = import.meta.env?.DEV ? await import('@rightkit/platform-ui/qa') : platformUi;
platform.configurePlatform({ fallbackPlatform: 'linux' });

function detect(): OS {
  if (import.meta.env?.DEV && 'bootstrapQaPlatform' in platform) {
    (platform as unknown as { bootstrapQaPlatform(input: { query?: string }): void }).bootstrapQaPlatform({
      query: typeof window !== 'undefined' ? window.location.search : '',
    });
  }
  const os = platform.getPlatform();
  return os === 'ios' || os === 'unknown' ? 'linux' : os;
}

export const platformOS: OS = detect();
export const isMac = platformOS === 'macos';
export const modKeyLabel = platform.formatChord('mod', platformOS === 'linux' ? 'windows' : platformOS)[0];

// Platform hook for CSS (font metrics, scrollbars). Set before first paint.
if (typeof document !== 'undefined') {
  document.documentElement.classList.add(`os-${platformOS}`);
}
