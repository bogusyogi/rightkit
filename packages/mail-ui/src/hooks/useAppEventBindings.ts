import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { createTrailingDebounce, type Debounced } from "../lib/debounce";
import {
  isAutostart,
  listOutbox,
  type OutboxRow,
  setUnreadBadge,
  syncAll,
} from "../lib/ipc";
import type { View } from "../store/types";

type AppEventBindingArgs = {
  activeAccount: string | null;
  signature: string;
  openCompose: () => void;
  setView: (view: View) => void;
  handleMailto: (url: string) => void;
  countsTotal: number;
  setAutostartOn: (enabled: boolean) => void;
  setOnline: (online: boolean) => void;
  appUnlocked: boolean;
  refresh: () => Promise<void>;
  setReloadKey: (updater: (key: number) => number) => void;
  setErr: (err: string | null) => void;
  view: View;
  setOutbox: (rows: OutboxRow[]) => void;
};

export function useAppEventBindings({
  activeAccount,
  signature,
  openCompose,
  setView,
  handleMailto,
  countsTotal,
  setAutostartOn,
  setOnline,
  appUnlocked,
  refresh,
  setReloadKey,
  setErr,
  view,
  setOutbox,
}: AppEventBindingArgs) {
  useEffect(() => {
    const uns: Array<() => void> = [];
    // The shell reveals the window; these only act on what the user picked. Tray items arrive
    // as `rightkit-shell://tray` ({kind: "menu", id}), macOS menu items as
    // `rightkit-shell://menu` ({id}); ids are set in src-tauri/src/desktop.rs.
    listen<{ kind: string; id?: string }>("rightkit-shell://tray", (e) => {
      if (e.payload?.kind === "menu" && e.payload.id === "compose") openCompose();
    })
      .then((u) => uns.push(u))
      .catch(() => {});
    listen<{ id: string }>("rightkit-shell://menu", (e) => {
      if (e.payload?.id === "menu-compose") openCompose();
      else if (e.payload?.id === "menu-settings") setView("settings");
    })
      .then((u) => uns.push(u))
      .catch(() => {});
    listen<string>("mailto-open", (e) => {
      if (typeof e.payload === "string") handleMailto(e.payload);
    })
      .then((u) => uns.push(u))
      .catch(() => {});
    return () => uns.forEach((u) => u());
    // eslint-disable-next-line react-hooks/exhaustive-deps -- openCompose/setView/handleMailto are recreated every render; tracking them would tear down and resubscribe these Tauri listeners on every render instead of only on account/signature change.
  }, [activeAccount, signature]);

  // At most one badge IPC per 500 ms, always carrying the latest count.
  const badgeCountRef = useRef(countsTotal);
  const badgeSendRef = useRef<Debounced | null>(null);
  if (badgeSendRef.current === null) {
    badgeSendRef.current = createTrailingDebounce(() => {
      setUnreadBadge(badgeCountRef.current).catch(() => {});
    }, 500);
  }
  useEffect(() => {
    badgeCountRef.current = countsTotal;
    badgeSendRef.current?.();
  }, [countsTotal]);
  useEffect(() => () => badgeSendRef.current?.cancel(), []);

  useEffect(() => {
    isAutostart()
      .then(setAutostartOn)
      .catch(() => {});
  }, [setAutostartOn]);

  useEffect(() => {
    const on = () => {
      setOnline(true);
      if (!appUnlocked) return;
      syncAll()
        .then(() => {
          refresh();
          setReloadKey((k) => k + 1);
        })
        .catch((e) => setErr(String(e)));
    };
    const off = () => setOnline(false);
    window.addEventListener("online", on);
    window.addEventListener("offline", off);
    return () => {
      window.removeEventListener("online", on);
      window.removeEventListener("offline", off);
    };
  }, [appUnlocked, refresh, setErr, setOnline, setReloadKey]);

  useEffect(() => {
    if (view === "settings" && appUnlocked)
      listOutbox()
        .then(setOutbox)
        .catch(() => {});
  }, [view, appUnlocked, setOutbox]);

}
