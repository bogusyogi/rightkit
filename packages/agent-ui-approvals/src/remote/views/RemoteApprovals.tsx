import { useCallback, useEffect, useState } from "react";
import { Button, Input } from "@rightkit/react-shell/react";
import { pendingApprovals, remoteApprovalsApi, remoteUnlockApi, type RemoteApproval } from "@rightkit/system-ui/api";
import { SettingRow, SettingsItem } from "@rightkit/settings-ui/core";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";

/** How often the list is re-read while Settings is open, so a new request shows up without a reload. */
const REFRESH_MS = 5000;

/** The name a person sees for what asks: the tool, or the terminal family for a session grant. */
function toolLabel(a: RemoteApproval): string {
  if (a.kind === "session" && a.scope === "unlock") return "Unlock a chat";
  if (a.kind === "session" && a.scope === "terminal") return "Terminal session";
  if (a.kind === "session" && a.scope.startsWith("launch:")) return "Open app (standing)";
  if (a.kind === "session" && a.scope.startsWith("fill:")) return "Form fill (standing)";
  return a.scope;
}

/** The door a request came by, in words: this computer's own tools, or ChatGPT through the gateway. */
function doorLabel(origin: string): string {
  return origin === "gateway" ? "ChatGPT (gateway)" : "This computer (pulse)";
}

const errorText = (e: unknown): string => (typeof e === "string" ? e : e instanceof Error ? e.message : String(e));

/** An unlock request: a chat asking to act on this computer, answered with the the host app PIN, the notch, or Deny. */
const isUnlock = (a: RemoteApproval): boolean => a.scope === "unlock";

/** The engine's unlock refusals arrive as "code: message"; show the message, and point to the PIN row when none is set. */
function unlockErrorText(e: unknown): string {
  const text = errorText(e);
  const m = /^(pin_not_set|pin_wrong|pin_locked|pin_invalid|approval_changed|unlock_needs_auth): (.*)$/s.exec(text);
  if (!m) return text;
  return m[1] === "pin_not_set" ? "No Pulse PIN is set. Set one in Settings > General > Remote access > Pulse PIN, or unlock from the Pulse notch." : m[2];
}

/**
 * Settings > Remote: the requests waiting for the owner's answer. Each one is a change to this computer
 * (a file change, a terminal session) that the remote tools asked for and that does not run until
 * Approve. Refuses clearly when the host app's engine is not attached: nothing is approved from this page
 * without it.
 */
export function RemoteApprovals() {
  const [items, setItems] = useState<RemoteApproval[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(() => {
    remoteApprovalsApi
      .pending()
      .then((answer) => {
        setItems(pendingApprovals(answer));
        setError(null);
      })
      .catch((e: unknown) => {
        setItems([]);
        setError(errorText(e));
      });
  }, []);

  useEffect(() => {
    load();
    const timer = visibleInterval(load, REFRESH_MS);
    return () => clearVisibleInterval(timer);
  }, [load]);

  // The PIN being typed per request id; held only while typed, cleared on submit, never logged.
  const [pins, setPins] = useState<Record<string, string>>({});
  const [pinSet, setPinSet] = useState<boolean | null>(null);

  useEffect(() => {
    if (!items.some(isUnlock)) return;
    remoteUnlockApi.pinStatus()
      .then((r) => setPinSet(Boolean((r as { set?: unknown } | null)?.set)))
      .catch(() => setPinSet(null));
  }, [items]);

  const decide = (a: RemoteApproval, approve: boolean, pin?: string) => {
    setBusy(a.id);
    setPins((p) => ({ ...p, [a.id]: "" }));
    (isUnlock(a) ? remoteUnlockApi.decide(a, approve, pin) : remoteApprovalsApi.decide(a, approve, pin))
      .then(() => { setError(null); })
      .catch((e: unknown) => { setError(unlockErrorText(e)); })
      .finally(() => {
        setBusy(null);
        load();
      });
  };

  return (
    <>
      <SettingRow className="ck-set" label="Waiting for your approval"
        description="Changes that remote tools asked to make here: a file change or a terminal session. Nothing runs until you approve it. A request expires after 10 minutes, and an approval is used once.">
        <Button size="sm" variant="ghost" onClick={load}>Refresh</Button>
      </SettingRow>
      {error !== null && (
        <SettingRow className="ck-set" label="Approvals unavailable" description={error}>{null}</SettingRow>
      )}
      {error === null && items.length === 0 && (
        <SettingRow className="ck-set" label="Nothing is waiting" description="A request appears here when a remote tool needs your approval.">{null}</SettingRow>
      )}
      {items.map((a) => (
        <SettingsItem key={a.id} text={`${toolLabel(a)} ${a.summary}`} className="ck-set ck-stack">
          <div className="ck-text">
            <strong>{toolLabel(a)}</strong>
            {/* The whole summary, wrapped; a long one scrolls inside this box so Approve and Deny stay in view. */}
            <div className="ck-approval-summary" role="region" tabIndex={0} aria-label={`${toolLabel(a)} request`}>{a.summary}</div>
            <span className="ck-sub">From {doorLabel(a.origin)}{a.client ? ` (${a.client})` : ""}</span>
          </div>
          {isUnlock(a) ? (
            <div className="ck-ctl">
              {pinSet === false ? (
                <span className="ck-sub">No Pulse PIN is set. Set one in the Pulse PIN row above, or unlock from the Pulse notch with Touch ID or Windows Hello.</span>
              ) : (
                <>
                  <Input type="password" className="ck-input" aria-label="Pulse PIN" placeholder="Pulse PIN"
                    autoComplete="off" autoCorrect="off" autoCapitalize="off" spellCheck={false}
                    value={pins[a.id] ?? ""}
                    onChange={(e) => { const v = e.target.value; setPins((p) => ({ ...p, [a.id]: v })); }}
                    onKeyDown={(e) => { if (e.key === "Enter" && busy === null && (pins[a.id] ?? "") !== "") decide(a, true, pins[a.id]); }} />
                  <Button size="sm" variant="primary" disabled={busy !== null || (pins[a.id] ?? "") === ""}
                    onClick={() => decide(a, true, pins[a.id])}>Unlock with PIN</Button>
                </>
              )}
              <Button size="sm" variant="secondary" disabled={busy !== null} onClick={() => decide(a, false)}>Deny</Button>
              <span className="ck-sub">or unlock from the Pulse notch with Touch ID or Windows Hello</span>
            </div>
          ) : (
            <div className="ck-ctl">
              <Input type="password" className="ck-input" aria-label="Pulse PIN" placeholder="Pulse PIN"
                autoComplete="off" autoCorrect="off" autoCapitalize="off" spellCheck={false}
                value={pins[a.id] ?? ""}
                onChange={(e) => { const v = e.target.value; setPins((p) => ({ ...p, [a.id]: v })); }}
                onKeyDown={(e) => { if (e.key === "Enter" && busy === null && (pins[a.id] ?? "") !== "") decide(a, true, pins[a.id]); }} />
              <Button size="sm" variant="primary" disabled={busy !== null || (pins[a.id] ?? "") === ""}
                onClick={() => decide(a, true, pins[a.id])}>Approve with PIN</Button>
              <Button size="sm" variant="secondary" disabled={busy !== null} onClick={() => decide(a, false)}>Deny</Button>
            </div>
          )}
        </SettingsItem>
      ))}
    </>
  );
}
