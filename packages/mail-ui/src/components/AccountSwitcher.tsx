// The account switcher — a 40px-tall control (avatar + caret) with a popover
// listing accounts + per-account notification mute. Lives at the right edge of
// each app's top bar (mail top bar, Drive/Calendar/Contacts bars) so switching
// accounts is reachable everywhere without crowding the titlebar. Self-contained:
// reads everything from the store, so it takes no props.
import Icon from "./Icons";
import { memo, useRef } from "react";
import { Button, IconButton, Popover } from "@rightkit/react-shell/react";
import { initialOf, unreadFor } from "../lib/mail";
import { setPref } from "../lib/preferences";
import { useAppStore } from "../store";

export const AccountSwitcher = memo(function AccountSwitcher() {
  const accounts = useAppStore((s) => s.accounts);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const counts = useAppStore((s) => s.counts);
  const switcherOpen = useAppStore((s) => s.switcherOpen);
  const setSwitcherOpen = useAppStore((s) => s.setSwitcherOpen);
  const setActiveAccount = useAppStore((s) => s.setActiveAccount);
  const setOpenThread = useAppStore((s) => s.setOpenThread);
  const notifyAccounts = useAppStore((s) => s.notifyAccounts);
  const setNotifyAccounts = useAppStore((s) => s.setNotifyAccounts);

  // Popover ignores outside presses on its anchor, so the pill's own toggle handler runs on a second press.
  const pillRef = useRef<HTMLButtonElement>(null);

  const activeAcc = accounts.find((a) => a.id === activeAccount) ?? null;
  const unreadForLocal = (id: string) => unreadFor(counts, id);

  // Stay in the current app (Drive/Calendar/…) when switching accounts; just
  // swap the account + drop any open thread that belonged to the old one. The
  // per-view effects reload their data for the new account.
  function pick(id: string) {
    setActiveAccount(id);
    setSwitcherOpen(false);
    setOpenThread(null);
  }

  return (
    <div className="switch-wrap acct-switcher">
      <Button variant="ghost"
        ref={pillRef}
        className="acct-pill"
        onClick={() => setSwitcherOpen((o) => !o)}
        disabled={accounts.length === 0}
        title={activeAcc?.display_name ?? "No account"}
      >
        {activeAcc ? (
          <span className="avatar sm" style={{ background: activeAcc.color }}>{initialOf(activeAcc.display_name)}</span>
        ) : (
          <span className="avatar sm avatar-empty">+</span>
        )}
        <span className="acct-pill-name">{activeAcc?.display_name ?? "No account"}</span>
        <Icon name="chevron-down" size={14} className="acct-caret" />
      </Button>
      {switcherOpen && accounts.length > 0 && (
        <Popover anchor={pillRef.current} onClose={() => setSwitcherOpen(false)} label="Accounts" placement="bottom-end" width={300}>
            {accounts.map((a) => (
              <div className={`switch-item ${a.id === activeAccount ? "active" : ""}`} key={a.id}>
                <Button variant="ghost"
                  type="button"
                  className="switch-pick"
                  style={{ display: "flex", alignItems: "center", gap: "inherit", flex: "1 1 auto", minWidth: 0, padding: 0, border: 0, background: "none", color: "inherit", font: "inherit", textAlign: "left", cursor: "pointer" }}
                  onClick={() => pick(a.id)}
                >
                  <span className="avatar sm" style={{ background: a.color }}>{initialOf(a.display_name)}</span>
                  <span className="si-name">{a.id}</span>
                </Button>
                {a.last_error ? (
                  <span className="acct-err" title={a.last_error}>⚠</span>
                ) : (
                  unreadForLocal(a.id) > 0 && <span className="badge">{unreadForLocal(a.id)}</span>
                )}
                <IconButton label={notifyAccounts[a.id] === false ? "Unmute notifications" : "Mute notifications"}
                  className={`notif-bell ${notifyAccounts[a.id] === false ? "muted" : ""}`}
                  aria-label={notifyAccounts[a.id] === false ? "Unmute notifications" : "Mute notifications"}
                  onClick={(e) => {
                    e.stopPropagation();
                    const next = { ...notifyAccounts, [a.id]: notifyAccounts[a.id] === false };
                    setNotifyAccounts(next);
                    setPref("notifyAccounts", next);
                  }}
                >
                  <Icon name={notifyAccounts[a.id] === false ? "bell-off" : "bell"} size={14} />
                </IconButton>
              </div>
            ))}
        </Popover>
      )}
    </div>
  );
});
