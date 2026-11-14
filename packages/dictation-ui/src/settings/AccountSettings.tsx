// Account section — HR-ACCOUNT-P1..P8 (docs/DOGFOOD.md). Passwordless
// Passwordless account: magic-link sign-in, sign-out, and account deletion.
// Device list/rename/remove/reassign lives in DeviceManagementSettings.tsx,
// a separate component so this file stays about session identity only.
//
// HR-ACCOUNT-P1/P5: Free/local use is accountless. Nothing on this page is
// reachable from onboarding, dictation, or the Free-tier code path — it is
// mounted only on the About page, alongside License, as an opt-in surface a
// Pro user visits to protect a purchase across devices or manage recovery.
// isPro()/getLicenseLifecycle() (lib/license.ts) never read anything from
// this file's account-session calls, so a signed-out or expired account
// cannot suppress an already-verified Pro entitlement (the structural half
// of HR-ACCOUNT-P4 — see LicenseSettings.tsx for the App Store purchase
// side of that guarantee).
//
// HR-ACCOUNT-P2/P3: passwordless only. The single input on this page is an
// email address for a magic link; there is no password field anywhere in
// this component, and none may be added to it.

import { memo, useCallback, useEffect, useState, type ReactNode } from "react";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import ErrorBanner from "../components/ErrorBanner.js";
import { hostAppName } from "../host.js";
import { cx, ui } from "../lib/ui.js";
import { sectionClient, type AccountClient, type AccountSessionStatus } from "./client.js";

// A row of the grouped settings card; `settingId` is the settings-search target.
function Row({ label, sublabel, settingId, children }: { label: string; sublabel?: string; settingId?: string; children: ReactNode }) {
  return (
    <SettingRow id={settingId} label={label} description={sublabel}>
      {children}
    </SettingRow>
  );
}

// The sign-in section needs the whole session surface, not just the device list.
type SessionClient = Required<Pick<AccountClient, "sessionStatus" | "refresh" | "beginMagicLink" | "complete" | "signOut" | "deleteAccount">>;
function sessionClient(): SessionClient & AccountClient {
  const account = sectionClient("account");
  for (const method of ["refresh", "beginMagicLink", "complete", "signOut", "deleteAccount"] as const) {
    if (!account[method]) {
      throw new Error(`@rightkit/dictation-ui: the DictationClient \`account\` group has no \`${method}\` (needed by the account section)`);
    }
  }
  if (!account.manageUrl) {
    throw new Error("@rightkit/dictation-ui: the DictationClient `account` group has no `manageUrl` (needed by the account section)");
  }
  return account as SessionClient & AccountClient;
}

// account_session.rs's AccountSessionStatus.expiresAt is Unix SECONDS
// (issued_at_unix + SESSION_TTL_SECONDS), not an ISO string.
function formatExpiry(unixSeconds: number | null | undefined): string {
  if (!unixSeconds) return "";
  try {
    return new Date(unixSeconds * 1000).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  } catch {
    return String(unixSeconds);
  }
}

function AccountSettingsImpl({ bare = false }: { bare?: boolean } = {}) {
  const account = sessionClient();
  const appName = hostAppName();
  // The account's web page: the link target, and its host and path in the sign-in explanation.
  const manageUrl = account.manageUrl as string;
  const manage = new URL(manageUrl);
  const manageText = `${manage.host}${manage.pathname.replace(/\/$/, "")}`;
  const [status, setStatus] = useState<AccountSessionStatus | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [email, setEmail] = useState("");
  const [linkSent, setLinkSent] = useState(false);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");

  const [deleteConfirming, setDeleteConfirming] = useState(false);
  const license = sectionClient("license");
  const [lifecycle, setLifecycle] = useState(license.monitor.get);
  useEffect(() => license.monitor.subscribe(setLifecycle), [license]);
  // Legacy StoredLicense path also counts as Pro (isPro() falls through to
  // the signed lifecycle regardless), kept only so this reads the same way
  // LicenseSettings.tsx derives `pro`.
  const pro = lifecycle.kind === "checking" ? license.isPro(null) : lifecycle.isPro;

  const refresh = useCallback(async () => {
    try {
      const s = await account.sessionStatus();
      setStatus(s);
      setLoadFailed(false);
      if (s.signedIn) {
        try {
          setStatus(await account.refresh());
        } catch {
          // Keep local state during transient failures. A rejected token is
          // cleared by Rust, so re-read once to reflect signed-out state.
          setStatus(await account.sessionStatus());
        }
      }
    } catch {
      // Command not yet registered (see evidence doc) or a transient
      // failure — either way this must read as "unknown", never as
      // "signed out", so the sign-in form and the signed-in panel are both
      // suppressed in favor of an honest status line.
      setLoadFailed(true);
    }
  }, [account]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function onSendLink() {
    const trimmed = email.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setError("");
    try {
      await account.beginMagicLink(trimmed);
      setLinkSent(true);
      setNote(`Check your email at ${trimmed} for a sign-in link.`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onCompleteToken() {
    const trimmed = token.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setError("");
    try {
      const s = await account.complete(trimmed);
      setStatus(s);
      setToken("");
      setLinkSent(false);
      setEmail("");
      setNote("Signed in.");
    } catch (e) {
      // A failed completion must never be shown as signed in — the sign-in
      // form stays up so the user can retry, matching the
      // never-render-failure-as-success standard HR-HISTORY-011A sets for
      // destructive History actions and this lane extends to sign-in.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onSignOut() {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await account.signOut();
      await refresh();
      setNote("Signed out. Your Pro entitlement on this device is unaffected.");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const signedIn = status?.signedIn === true;

  // HR-ACCOUNT-P8: `DELETE /v1/shop/account` is real (`routes/account.ts`)
  // and `account_session.rs`'s `account_delete` command calls it directly —
  // this is a real destructive action, not a support request. Never render
  // a rejected call as success: on failure `deleteAccount()` throws and
  // `status`/`deleteConfirming` are left untouched so the confirmation panel
  // stays up with the error shown, matching the HR-HISTORY-011A standard
  // this lane already follows for sign-in and device removal.
  async function onDeleteAccount() {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await account.deleteAccount();
      // The account this session authenticated no longer exists — clear the
      // local session state immediately rather than waiting on a refresh()
      // round trip (server already reflects this; account_delete's Rust
      // side already cleared the keychain record).
      setDeleteConfirming(false);
      setStatus({ signedIn: false, email: null, expiresAt: null });
      setNote(`Account deleted. Your ${appName} product profile and devices are gone; anonymized purchase records are retained for accounting and legal requirements.`);
    } catch (e) {
      // Left in the confirming state so the user can read the error and
      // retry or cancel — a failure must never look like it succeeded.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const body = (
    <>
      <ErrorBanner message={error} onDismiss={() => setError("")} />

      {loadFailed && (
        <p className={cx(ui.fieldNote, ui.fieldNoteGap)} role="status">
          Account status is unavailable right now. Free use, local dictation, and any Pro entitlement
          already active on this device are unaffected.
        </p>
      )}

      {!loadFailed && signedIn && (
        <Row label="Status" settingId="about.status">
          <span className={ui.keyHint}>
            {status === null ? "Checking…" : signedIn ? `Signed in · ${status.email ?? ""}` : "Not signed in"}
          </span>
        </Row>
      )}

      {!loadFailed && signedIn && status?.expiresAt && (
        <Row label="Session" settingId="about.session">
          <span className={ui.mutedSm}>Valid through {formatExpiry(status.expiresAt)}</span>
        </Row>
      )}

      {!loadFailed && !signedIn && (
        <>
          <Row
            label="Sign in"
            settingId="about.sign-in"
            sublabel={
              pro
                ? `Optional. Links this Pro purchase to an account so you can see every device using it, remove a lost or replaced one, and reactivate on a new one. Passwordless: we email a one-time link. The same account works at ${manageText}.`
                : `Optional. An account shows the devices on your license and lets you remove one to make room for another. Passwordless: we email a one-time link. The same account works at ${manageText}.`
            }
          >
            {!linkSent ? (
              <div className="flex items-center gap-2">
                <input
                  className={cx(ui.input, "flex-1")}
                  type="email"
                  inputMode="email"
                  autoComplete="email"
                  spellCheck={false}
                  placeholder="you@example.com"
                  aria-label="Email for account sign-in"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void onSendLink();
                  }}
                  disabled={busy}
                />
                <button
                  type="button"
                  className={ui.button}
                  onClick={() => void onSendLink()}
                  disabled={busy || !email.trim()}
                >
                  {busy ? "Sending…" : "Send link"}
                </button>
              </div>
            ) : (
              <span className={ui.keyHint}>Link sent</span>
            )}
          </Row>

          {linkSent && (
            <Row label="Have a link token?" settingId="about.have-a-link-token" sublabel="Paste the token from the emailed link to finish signing in on this device.">
              <div className="flex items-center gap-2">
                <input
                  className={cx(ui.input, "flex-1")}
                  type="text"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="Sign-in token"
                  aria-label="Magic-link token"
                  value={token}
                  onChange={(e) => setToken(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void onCompleteToken();
                  }}
                  disabled={busy}
                />
                <button
                  type="button"
                  className={cx(ui.button, ui.buttonGhost)}
                  onClick={() => void onCompleteToken()}
                  disabled={busy || !token.trim()}
                >
                  Complete sign-in
                </button>
              </div>
            </Row>
          )}

        </>
      )}

      {!loadFailed && signedIn && (
        <>
          <Row label="Manage on the web" settingId="about.manage-on-the-web" sublabel="Devices, orders, and account settings for this account.">
            <a href={manageUrl} className={cx(ui.button, ui.buttonGhost)}>
              Open {manage.host}
            </a>
          </Row>
          <Row label="Sign out" settingId="about.sign-out">
            <button type="button" className={cx(ui.button, ui.buttonGhost)} onClick={() => void onSignOut()} disabled={busy}>
              Sign out
            </button>
          </Row>

          {!deleteConfirming ? (
            <Row
              label="Delete account"
              settingId="about.delete-account"
              sublabel="Permanently deletes your account profile and its device list. Purchase records are anonymized and kept for receipts, refunds, and tax records."
            >
              <button
                type="button"
                className={cx(ui.button, ui.buttonGhost)}
                onClick={() => setDeleteConfirming(true)}
                disabled={busy}
              >
                Delete account
              </button>
            </Row>
          ) : (
            <Row label="Delete account — are you sure?" settingId="about.delete-account-confirm">
              <div className="flex flex-col gap-2">
                <p className={ui.mutedSm}>
                  This deletes: your account profile, email, and every device linked to this account. This
                  keeps: your order and purchase history, anonymized (customer link removed) so receipts,
                  refunds, and tax/legal records survive. This cannot be undone.
                </p>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    className={cx(ui.button, ui.buttonDanger, ui.buttonCompact)}
                    onClick={() => void onDeleteAccount()}
                    disabled={busy}
                  >
                    {busy ? "Deleting…" : "Confirm delete"}
                  </button>
                  <button
                    type="button"
                    className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
                    onClick={() => setDeleteConfirming(false)}
                    disabled={busy}
                  >
                    Cancel
                  </button>
                </div>
              </div>
            </Row>
          )}
        </>
      )}

      {note && (
        <p className={cx(ui.fieldNote, ui.fieldNoteGap)} aria-live="polite">
          {note}
        </p>
      )}
    </>
  );

  if (bare) return body;
  return (
    <SettingsGroup title="Account">
      {body}
      <p className="m-0 mx-[14px] mt-[7px] text-[length:var(--size-xs)] leading-[1.45] text-[var(--muted)] [text-wrap:pretty]">
        Not needed for Free use or local dictation. Only for activating Pro on another platform or removing a lost device.
      </p>
    </SettingsGroup>
  );
}

export const AccountSettings = memo(AccountSettingsImpl);
