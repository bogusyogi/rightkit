// New-mail poll: every 60s refresh unread counts; on an increase, chime (if the
// sound is on) and surface a per-account desktop notification, and refresh the
// list when the user isn't reading/composing. Extracted from App. Owns the
// per-account + total "previous count" refs.
import { useEffect, useRef } from "react";
import { useAppStore } from "../store/index.js";
import { unreadCounts, notify } from "../lib/ipc.js";
import { playChime } from "../lib/sound.js";
import { hostAppName } from "../appName.js";

export function useNewMailPoll() {
  const openThread = useAppStore((s) => s.openThread);
  const compose = useAppStore((s) => s.compose);
  const notifyOn = useAppStore((s) => s.notifyOn);
  const notifyAccounts = useAppStore((s) => s.notifyAccounts);
  const soundOn = useAppStore((s) => s.soundOn);
  const setCounts = useAppStore((s) => s.setCounts);
  const setReloadKey = useAppStore((s) => s.setReloadKey);
  const prevByAcct = useRef<Record<string, number>>({});
  const prevUnread = useRef<number | null>(null);

  useEffect(() => {
    const id = window.setInterval(() => {
      // Skip IPC while the browser reports no network connectivity.
      if (!navigator.onLine) return;
      unreadCounts()
        .then((c) => {
          const prev = prevUnread.current;
          if (prev !== null && c.total > prev && soundOn) playChime();
          if (prev !== null && c.total > prev && notifyOn) {
            // Per-account routing: notify only for accounts whose count rose and
            // whose per-account notifications aren't switched off.
            const risen = c.per_account.filter(
              (p) =>
                p.count > (prevByAcct.current[p.account_id] ?? 0) &&
                notifyAccounts[p.account_id] !== false
            );
            const n = c.total - prev;
            const title = risen.length === 1 ? risen[0].account_id : hostAppName();
            const body = `${n} new message${n > 1 ? "s" : ""}`;
            if (risen.length > 0) {
              // The native toast (rightkit-tauri-shell notify) is the only path; the shell owns
              // permission and the app id, so there is no web Notification fallback.
              notify(title, body).catch(() => {});
            }
            if (!openThread && !compose) setReloadKey((k) => k + 1);
          }
          for (const p of c.per_account) prevByAcct.current[p.account_id] = p.count;
          prevUnread.current = c.total;
          setCounts(c);
        })
        .catch(() => {});
    }, 60000);
    return () => window.clearInterval(id);
  }, [openThread, compose, notifyOn, notifyAccounts, soundOn, setCounts, setReloadKey]);
}
