import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../store";
import {
  archiveProgress,
  folderCount,
  getSignature,
  getVacation,
  listFilters,
  listForwardingAddresses,
  listLabels,
  writerBusy,
} from "../lib/ipc";
import { hasInboxCategories } from "../lib/mail";
import { createTrailingDebounce } from "../lib/debounce";
import { decideDeferredRefresh } from "../lib/deferredRefresh";
import {
  consumeSyncReload,
  createSyncReloadGate,
  noteSyncAccount,
  shouldReplayHiddenSync,
} from "../lib/syncReloadGate";
import { claimThreadLoad, type ThreadLoadGate } from "../lib/threadLoadGate";
import {
  isInitialRefreshSettled,
  subscribeInitialRefreshSettled,
} from "../lib/initialRefresh";

export function useAppDataEffects() {
  const view = useAppStore((s) => s.view);
  const settingsTab = useAppStore((s) => s.settingsTab);
  const appLock = useAppStore((s) => s.appLock);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const archiveProg = useAppStore((s) => s.archiveProg);
  const setArchiveProg = useAppStore((s) => s.setArchiveProg);
  const refreshAfterSync = useAppStore((s) => s.refreshAfterSync);
  const refreshStaticSettings = useAppStore((s) => s.refreshStaticSettings);
  const loadThreads = useAppStore((s) => s.loadThreads);
  const query = useAppStore((s) => s.query);
  const folder = useAppStore((s) => s.folder);
  const reloadKey = useAppStore((s) => s.reloadKey);
  const activeLabel = useAppStore((s) => s.activeLabel);
  const unreadOnly = useAppStore((s) => s.unreadOnly);
  const category = useAppStore((s) => s.category);
  const setCategory = useAppStore((s) => s.setCategory);
  const labels = useAppStore((s) => s.labels);
  const setLabels = useAppStore((s) => s.setLabels);
  const setDraftCount = useAppStore((s) => s.setDraftCount);
  const setSignatureState = useAppStore((s) => s.setSignatureState);
  const setVacation = useAppStore((s) => s.setVacation);
  const setForwarding = useAppStore((s) => s.setForwarding);
  const setFilters = useAppStore((s) => s.setFilters);
  const setSelected = useAppStore((s) => s.setSelected);
  const setFocusIdx = useAppStore((s) => s.setFocusIdx);
  const setNoMoreOlder = useAppStore((s) => s.setNoMoreOlder);
  const clearUndo = useAppStore((s) => s.clearUndo);
  const refreshSettled = useSyncExternalStore(
    subscribeInitialRefreshSettled,
    isInitialRefreshSettled,
  );
  // Account whose labels fetch has settled ("" = no account). Keyed to the
  // account so a stale value from before the account arrived cannot count.
  const [labelsFor, setLabelsFor] = useState<string | null>(null);
  const labelsSettled = labelsFor === (activeAccount ?? "");
  const threadLoadGate = useRef<ThreadLoadGate>({ startupLoaded: false });

  const appUnlocked =
    !appLock || appLock.state === "disabled" || appLock.state === "unlocked";
  const isInboxView = !activeLabel && (folder === "all" || folder === "inbox");
  const hasCategories = hasInboxCategories(labels);

  useEffect(() => {
    if (view !== "settings" || settingsTab !== "privacy" || !activeAccount)
      return;
    let live = true;
    const load = () =>
      archiveProgress(activeAccount)
        .then((p) => live && setArchiveProg(p))
        .catch(() => {});
    load();
    const id = window.setInterval(() => {
      if (archiveProg?.active) load();
    }, 3000);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, [view, settingsTab, activeAccount, archiveProg?.active, setArchiveProg]);

  useEffect(() => {
    let live = true;
    const gate = createSyncReloadGate();
    // L13: writer-busy refresh skip. When the debounce fires right as the
    // writer connection is mid-transaction, firing the refresh anyway just
    // queues the refresh's reads behind that write — no faster than waiting
    // one more debounce cycle, and it adds load exactly when the writer is
    // already busy. `deferredOnce` caps this to a SINGLE extra cycle per
    // scheduled fire so a persistently busy writer can't starve the refresh
    // forever (mirrors the backend's non-looping reader-reopen contract).
    let deferredOnce = false;
    const fire = () => {
      if (!live) return;
      if (!consumeSyncReload(gate, document.visibilityState, appUnlocked)) return;
      writerBusy()
        .catch(() => false)
        .then((busy) => {
          if (!live) return;
          const decision = decideDeferredRefresh(busy, deferredOnce);
          deferredOnce = decision.deferredOnce;
          if (decision.action === "defer") {
            schedule();
            return;
          }
          refreshAfterSync().catch(() => {});
          if (!query.trim()) loadThreads();
        });
    };
    // 400ms trailing debounce: a burst of per-account sync-complete events
    // coalesces into one refresh instead of one per account.
    const schedule = createTrailingDebounce(fire, 400);
    const onVisibility = () => {
      if (shouldReplayHiddenSync(gate, document.visibilityState)) schedule();
    };
    const unlistenVis = listen<{ account_id: string; changed: number; ok: boolean }>(
      "mailright-sync-complete",
      (event) => {
        if (!live || !event.payload.ok || event.payload.changed === 0) return;
        noteSyncAccount(gate, event.payload.account_id);
        schedule();
      },
    ).catch(() => null);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      live = false;
      document.removeEventListener("visibilitychange", onVisibility);
      schedule.cancel();
      if (unlistenVis) unlistenVis.then((u) => { if (u) u(); }).catch(() => {});
    };
  }, [refreshAfterSync, loadThreads, query, appUnlocked]);

  // Static settings (client id/secret, image policy, tracker blocking,
  // destructive flag) cannot change because of a sync, so they are NOT part of
  // the sync-complete refresh. Re-read them when the Settings view closes,
  // which is the only place the user can change them.
  const prevView = useRef(view);
  useEffect(() => {
    const wasSettings = prevView.current === "settings";
    prevView.current = view;
    if (wasSettings && view !== "settings" && appUnlocked) {
      refreshStaticSettings().catch(() => {});
    }
  }, [view, appUnlocked, refreshStaticSettings]);

  useEffect(() => {
    setSelected(new Set());
    setFocusIdx(-1);
    setNoMoreOlder(false);
    // The first load waits for the initial refresh and startup labels; later
    // triggers load as before. See src/lib/threadLoadGate.ts.
    if (
      claimThreadLoad(threadLoadGate.current, {
        appUnlocked,
        categoryPending: isInboxView && hasCategories && category === null,
        hasQuery: query.trim().length > 0,
        refreshSettled,
        labelsSettled,
      })
    )
      loadThreads();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- loadThreads is a store action with unstable identity across renders; adding it here would refire the load on every render, not just on scope/filter changes.
  }, [
    folder,
    activeAccount,
    reloadKey,
    query,
    category,
    activeLabel,
    unreadOnly,
    isInboxView,
    hasCategories,
    appUnlocked,
    refreshSettled,
    labelsSettled,
  ]);

  useEffect(() => {
    if (isInboxView && hasCategories && category === null)
      setCategory("PRIMARY");
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setCategory is a store setter with unstable identity; tracking it would refire this effect every render instead of only on the three real inputs.
  }, [isInboxView, hasCategories, category]);

  useEffect(() => {
    if (!appUnlocked || !activeAccount) {
      setLabels([]);
      setDraftCount(0);
      setLabelsFor("");
      return;
    }
    listLabels(activeAccount)
      .then(setLabels)
      .catch(() => setLabels([]))
      .finally(() => setLabelsFor(activeAccount));
    folderCount("DRAFT", activeAccount)
      .then(setDraftCount)
      .catch(() => setDraftCount(0));
    getSignature(activeAccount)
      .then(setSignatureState)
      .catch(() => setSignatureState(""));
  }, [activeAccount, appUnlocked, reloadKey, setDraftCount, setLabels, setSignatureState]);

  useEffect(() => {
    if (view !== "settings" || !activeAccount) return;
    getVacation(activeAccount)
      .then(setVacation)
      .catch(() => setVacation(null));
    listForwardingAddresses(activeAccount)
      .then(setForwarding)
      .catch(() => setForwarding([]));
    listFilters(activeAccount)
      .then(setFilters)
      .catch(() => setFilters([]));
  }, [view, activeAccount, setFilters, setForwarding, setVacation]);

  useEffect(() => () => clearUndo(), [clearUndo]);
}
