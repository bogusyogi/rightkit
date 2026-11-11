// Hub-level Accessibility banner. Onboarding can be "done" yet Accessibility be
// revoked (e.g. after installing a new signed build, which macOS treats as a new
// identity). The hub re-checks on mount and focus. It polls only while access is
// absent, so a healthy app does not make synchronous AX IPC every two seconds.
import { useEffect, useRef, useState } from "react";
import { dictationClient } from "../host.js";
import { getPlatform } from "@rightkit/platform-ui";
import { hostAppName } from "../host.js";

export default function AccessibilityBanner() {
  const platform = getPlatform();
  // Assume granted until the first check resolves, so the banner never flashes.
  const [granted, setGranted] = useState(true);
  const shortcutsActivated = useRef(false);

  useEffect(() => {
    if (platform !== "macos") return;
    let alive = true;
    const check = () =>
      (dictationClient().accessibilityGranted?.(false) ?? Promise.resolve(true))
        .then((ok) => {
          if (!alive) return;
          setGranted(ok);
          if (ok && !shortcutsActivated.current) {
            shortcutsActivated.current = true;
            void (dictationClient().activateShortcuts?.() ?? Promise.resolve()).catch(() => {
              shortcutsActivated.current = false;
            });
          } else if (!ok) {
            shortcutsActivated.current = false;
          }
        })
        .catch(() => undefined);
    void check();
    const id = granted ? undefined : window.setInterval(check, 2000);
    const onFocus = () => void check();
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      alive = false;
      if (id !== undefined) window.clearInterval(id);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [granted, platform]);

  if (platform !== "macos" || granted) return null;

  return (
    <div className="flex flex-wrap items-center justify-between gap-[var(--space-3)] border-b border-[color-mix(in_srgb,var(--accent),transparent_55%)] bg-[color-mix(in_srgb,var(--accent),var(--surface-1)_85%)] px-[var(--space-4)] py-[var(--space-2)] text-[length:var(--size-sm)]">
      <span className="text-[var(--paper)]">
        Keyboard shortcuts are off — {hostAppName()} needs <b>Accessibility</b> access to receive fn
        shortcuts and paste back into your app.
      </span>
      <button
        type="button"
        onClick={() => void dictationClient().openAccessibilitySettings?.().catch(() => undefined)}
        className="shrink-0 cursor-pointer rounded-[var(--radius-sm)] bg-[var(--accent)] px-[var(--space-3)] py-[6px] font-medium text-[var(--accent-contrast)] transition-[filter] hover:brightness-110 focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
      >
        Open Settings
      </button>
    </div>
  );
}
