import { useEffect, useLayoutEffect } from "react";
import { toast } from "@rightkit/platform-ui/headless";
import { logEvent, setDiagnosticsEnabled } from "@rightkit/document-core/state/diagnostics";
import { getPersistenceStatus } from "@rightkit/document-core/state/tauriBridge";
import { scheduleAfterPaint } from "@rightkit/document-core/app/deferredMount";
import { syncActiveFormatSavers } from "./activeFormatSavers.js";
import { hostAppName } from "../appName.js";
import { documentEvent } from "@rightkit/document-core/host";

type SyncArgs = Parameters<typeof syncActiveFormatSavers>;

/** Small self-contained App effects, extracted verbatim: diagnostics toggle, startup persistence
 * warning, pane-active paint marker, active-format saver sync, and the native-menu Settings event. */
export function useAppEffects({
  diagnosticsSetting,
  activePath,
  docxSaver,
  imageSaver,
  docxSaversRef,
  imageSaversRef,
  openSettings,
}: {
  diagnosticsSetting: boolean | undefined;
  activePath: SyncArgs[0];
  docxSaver: SyncArgs[1];
  imageSaver: SyncArgs[2];
  docxSaversRef: { current: SyncArgs[3] };
  imageSaversRef: { current: SyncArgs[4] };
  openSettings: () => void;
}) {
  // diagnostics.ts caches the privacy toggle instead of re-parsing settings per event; keep it current.
  useEffect(() => {
    setDiagnosticsEnabled(diagnosticsSetting !== false);
  }, [diagnosticsSetting]);
  // Startup: warn persistently when the local database was unreadable and had to be replaced.
  useEffect(() => {
    void (async () => {
      try {
        const status = await getPersistenceStatus();
        if (status.degraded) {
          toast(
            status.quarantinedTo
              ? `${hostAppName()} couldn't read its local database, so recent files, pins and tabs start empty. The original was kept at ${status.quarantinedTo}.`
              : `${hostAppName()} couldn't open its local database; recent files, tabs and settings won't be saved this session.`,
            { kind: "warning", ttl: 0 },
          );
        }
      } catch {
        // Status probe is best-effort; a failed probe must not block startup.
      }
    })();
  }, []);
  // Post-paint marker — independent of resource reconciliation. The marker effect's
  // deps are [activePath] only so the cleanup-then-rerun pattern in the deferred-mount effect
  // (which cancels its own rAFs when renderedPaths updates) does not also cancel this log.
  // Fires for both cold & warm clicks as a "pane became active" latency marker.
  useEffect(() => {
    if (!activePath) return;
    return scheduleAfterPaint(() => {
      logEvent("pane:active:paint");
    });
  }, [activePath]);
  // Restore an already-mounted DOCX/image tab's registered saver before paint. Clearing both
  // refs loses warm viewers because their registration effect correctly does not remount.
  useLayoutEffect(() => {
    syncActiveFormatSavers(
      activePath,
      docxSaver,
      imageSaver,
      docxSaversRef.current,
      imageSaversRef.current,
    );
  }, [activePath, docxSaver, docxSaversRef, imageSaver, imageSaversRef]);
  // macOS app menu: the host app › Settings… (⌘,) fires this from the native menu bridge.
  useEffect(() => {
    window.addEventListener(documentEvent("open-settings"), openSettings);
    return () => window.removeEventListener(documentEvent("open-settings"), openSettings);
  }, [openSettings]);
}
