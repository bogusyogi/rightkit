import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { archiveProgress, type ArchiveProgress } from "../lib/ipc.js";

/**
 * Backs the search-coverage honesty banner (Lane C2). Fetches ArchiveProgress
 * lazily the first time a search executes for the active account, caches it
 * for the session, and refreshes it when a sync completes (the closest signal
 * to "download-all made progress" this app currently emits — archive_progress
 * is recomputed from live rows on refetch either way). Dismissal is
 * per-session and keyed to the account so switching accounts re-shows it.
 */
export function useSearchCoverage({
  activeAccount,
  searchActive,
}: {
  activeAccount: string | null;
  searchActive: boolean;
}) {
  const [progress, setProgress] = useState<ArchiveProgress | null>(null);
  const [dismissedAccount, setDismissedAccount] = useState<string | null>(null);
  const fetchedForAccount = useRef<string | null>(null);

  useEffect(() => {
    if (!searchActive || !activeAccount) return;
    if (fetchedForAccount.current === activeAccount) return;
    fetchedForAccount.current = activeAccount;
    archiveProgress(activeAccount)
      .then(setProgress)
      .catch(() => {});
  }, [searchActive, activeAccount]);

  useEffect(() => {
    if (!activeAccount) return;
    let live = true;
    const unlistenPromise = listen<{ account_id: string; ok: boolean }>(
      "mailright-sync-complete",
      (event) => {
        if (!live) return;
        if (event.payload.account_id !== activeAccount || !event.payload.ok) return;
        if (fetchedForAccount.current !== activeAccount) return;
        archiveProgress(activeAccount)
          .then((p) => live && setProgress(p))
          .catch(() => {});
      },
    ).catch(() => null);
    return () => {
      live = false;
      unlistenPromise.then((u) => u && u()).catch(() => {});
    };
  }, [activeAccount]);

  const dismissed = dismissedAccount === activeAccount;
  const dismiss = () => setDismissedAccount(activeAccount);

  return {
    coverage: dismissed ? null : progress,
    dismissCoverageBanner: dismiss,
  };
}
