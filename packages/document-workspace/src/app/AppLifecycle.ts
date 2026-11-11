import { useEffect } from "react";
import { toast } from "@rightkit/platform-ui/headless";
import { addRecentUrl } from "@rightkit/document-core/state/recentUrls";
import { watchFiles, onAppEvent } from "@rightkit/document-core/state/tauriBridge";
import type { Unlisten } from "@rightkit/document-core/state/bridgeEvents";
import { hostAppName } from "../appName.js";
import { workspaceConfig } from "../config.js";
import { documentEvent } from "@rightkit/document-core/host";

/** Tauri's unlisten callback is asynchronous. During window teardown its backend listener may
 * already be gone, so contain either sync or async cleanup failures instead of leaking an
 * unhandled rejection from React effect cleanup. */
export function releaseAppEventListener(unlisten?: Unlisten): void {
  if (!unlisten) return;
  try {
    void Promise.resolve(unlisten()).catch(() => {});
  } catch {
    // Backend already disappeared during native window teardown.
  }
}

/**
 * Self-contained app-lifecycle effects, extracted from App.tsx: the file watcher re-arm on the open-tab/sub-page set, the one-shot
 * background update check gated on settings, and the "open URL as a tab" event wiring (custom
 * window event + the Rust documentEvent("open-url") bridge event). None of these read per-keystroke
 * state, so moving them here does not change render/perf characteristics.
 */
export function useAppLifecycle({
  watchKey,
  autoCheckUpdates,
  autoUpdateCheckRan,
  openLiveUrl,
}: {
  watchKey: string;
  autoCheckUpdates: boolean;
  autoUpdateCheckRan: React.MutableRefObject<boolean>;
  openLiveUrl: (url: string, opts?: { background?: boolean }) => Promise<unknown>;
}) {
  // Watch EVERY open file-backed tab plus each HTML session's visited sub-pages, in the one watcher.
  // An external edit to ANY of them (active or background) arrives via the `file-changed` event and
  // reloads that tab when it happens — so tab activation is a pure state change (no re-stat /
  // reload-on-switch) and a background tab is already fresh by the time you click it. Keyed on the
  // sorted path set so it re-arms only when a tab opens/closes/navigates, not on every keystroke.
  useEffect(() => {
    void watchFiles(watchKey ? watchKey.split("|") : []);
  }, [watchKey]);

  useEffect(() => {
    const check = workspaceConfig().backgroundUpdateCheck;
    if (!check || !autoCheckUpdates || autoUpdateCheckRan.current) return;
    autoUpdateCheckRan.current = true;
    let cancelled = false;
    void check()
      .then((update) => {
        if (!update || cancelled) return;
        // The host's check announces only updates the user can install; the rest stay in Settings.
        const tier = update.tier === "patch" ? "patch" : "update";
        toast(
          `${hostAppName()} ${update.version} ${tier} is ready. Open Settings > Updates to install.`,
          "info",
        );
      })
      .catch(() => {
        // Background update checks are advisory; Settings still permits a manual retry.
      });
    return () => {
      cancelled = true;
    };
  }, [autoCheckUpdates, autoUpdateCheckRan]);

  // Open an explicit URL preview as a VR web tab. Sources: the Open URL dialog/clipboard, a
  // new-window link in a URL preview (Rust → documentEvent("open-url") Tauri event), and web links
  // followed from document previews (bridge → documentEvent("open-url-tab")).
  useEffect(() => {
    const openUrlInTab = (url: string, background = false) => {
      addRecentUrl(url);
      void openLiveUrl(url, { background });
    };
    const onWindowOpen = (e: Event) => {
      const detail = (e as CustomEvent<string | { url: string; background?: boolean }>).detail;
      // Detail is a bare url (normal click → foreground) or { url, background } (new-tab gesture).
      if (typeof detail === "string" && detail) openUrlInTab(detail);
      else if (detail && typeof detail === "object" && detail.url)
        openUrlInTab(detail.url, !!detail.background);
    };
    window.addEventListener(documentEvent("open-url-tab"), onWindowOpen);

    let un: Unlisten | undefined;
    let cancelled = false;
    void onAppEvent("open-url", openUrlInTab).then((u) => {
      if (cancelled) releaseAppEventListener(u);
      else un = u;
    });
    return () => {
      cancelled = true;
      releaseAppEventListener(un);
      window.removeEventListener(documentEvent("open-url-tab"), onWindowOpen);
    };
  }, [openLiveUrl]);
}
