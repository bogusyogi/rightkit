// License section - enter a Pro key, activate, show plan. Self-managed state
// (the license lives in the OS keyring, not the settings blob).
// Free = local dictation; Pro = longer file transcription, more languages, and
// macOS Shortcuts. Everything native goes through `DictationClient.license`.

import { memo, useEffect, useState, type ReactNode } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import ErrorBanner from "../components/ErrorBanner.js";
import { hostAppName } from "../host.js";
import { cx, ui } from "../lib/ui.js";
import { sectionClient, type LicenseStorageState, type LicenseViewLike } from "./client.js";

// A row of the grouped settings card; `settingId` is the settings-search target.
function Row({ label, sublabel, settingId, children }: { label: string; sublabel?: string; settingId?: string; children: ReactNode }) {
  return (
    <SettingRow id={settingId} label={label} description={sublabel}>
      {children}
    </SettingRow>
  );
}

function LicenseSettingsImpl({ bare = false }: { bare?: boolean } = {}) {
  const host = sectionClient("license");
  const appName = hostAppName();
  const words = host.words ?? ((text: string) => text);
  const [license, setLicense] = useState<LicenseViewLike | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [storageState, setStorageState] = useState<LicenseStorageState>(
    host.storage.get,
  );
  const [lifecycle, setLifecycle] = useState(host.monitor.get);
  // Trial: a small in-app modal (email -> emailed key), NOT a jump to the
  // website. Same flow as first-run onboarding.
  const [trialOpen, setTrialOpen] = useState(false);
  const [trialEmail, setTrialEmail] = useState("");
  const [trialBusy, setTrialBusy] = useState(false);
  const [trialNote, setTrialNote] = useState("");

  // Apple-device Pro continuity (macOS only, Pro only): READ-ONLY status
  // from pro_bridge.rs. HR-SYNC-001 requires exactly ONE user-facing control
  // for this — the unified "Sync with Apple devices" toggle lives in
  // Application settings (AppleSyncSettings.tsx). This section is a status
  // line pointing at that control, not a second writable checkbox for the
  // same backend flag (the duplicate-control defect family HR-HISTORY-011
  // forbids in History).
  const isMac = getPlatform() === "macos";
  const isWindows = getPlatform() === "windows";
  // One trial length on every platform; the server grants the same.
  const trialDays = host.trialDays;

  // HR-PRO-015 Windows Apple-link: request a short-lived single-use code on
  // this already-leased Apple device (Mac), then redeem it on Windows. The
  // Rust reads current device-lease credentials from secure storage. App
  // Store buyers never receive a raw license key, so this flow never asks
  // them to manufacture or re-enter one.
  const [winLinkCode, setWinLinkCode] = useState<{ code: string; expiresAt: number } | null>(null);
  const [winLinkBusy, setWinLinkBusy] = useState(false);
  const [winLinkError, setWinLinkError] = useState("");
  const [winRedeemCodeInput, setWinRedeemCodeInput] = useState("");
  const [winRedeemBusy, setWinRedeemBusy] = useState(false);
  const [winRedeemError, setWinRedeemError] = useState("");
  const [winRedeemNote, setWinRedeemNote] = useState("");

  async function onRequestTrial() {
    const email = trialEmail.trim();
    if (!email || trialBusy) return;
    setTrialBusy(true);
    setTrialNote("");
    try {
      // The trial length depends on the platform; native code adds it.
      const ok = await host.requestTrialKey(email);
      if (ok) {
        setTrialNote("Key sent. Check your inbox, then paste it above and Activate.");
        setTrialEmail("");
      } else {
        setTrialNote("Couldn't send the key. Check the email and your connection, then try again.");
      }
    } finally {
      setTrialBusy(false);
    }
  }

  useEffect(() => {
    let alive = true;
    host.loadLicense()
      .then((l) => alive && setLicense(l))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => host.storage.subscribe(setStorageState), []);
  useEffect(() => host.monitor.subscribe(setLifecycle), []);

  const pro = lifecycle.kind === "checking" ? host.isPro(license) : lifecycle.isPro;


  async function onActivate() {
    const trimmed = keyInput.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setNote("");
    try {
      // Every new activation enters renewable server-authorized device-lease
      // lifecycle (native code decides). Existing legacy records remain
      // readable/refreshable, but new users must not be put back onto final
      // forever-local tokens.
      const result = await host.activateLicense(trimmed);
      setLicense(result);
      if (result.status === "active") {
        setKeyInput("");
        // `active` alone is not Pro: a licence for a lower tier is active too.
        setNote(host.viewShowsPro(result) ? "Pro activated." : "License activated.");
      } else {
        setNote(host.licenseText(result.message) || "License key was not accepted.");
      }
    } catch (error) {
      setLicense(null);
      if (host.storage.get().kind !== "error") {
        setNote(error instanceof Error ? words(error.message) : "License activation failed.");
      }
    } finally {
      setBusy(false);
    }
  }

  async function onRetryStorage() {
    if (busy) return;
    setBusy(true);
    setNote("");
    try {
      const restored = await host.retryLicenseStorage();
      setLicense(restored);
      setNote("Secure license storage is available again.");
    } catch {
      setLicense(null);
    } finally {
      setBusy(false);
    }
  }

  async function onRemove() {
    if (busy) return;
    setBusy(true);
    setNote("");
    try {
      // Suppresses this Mac's Apple-device Pro-bridge consumption BEFORE
      // deactivating, so it cannot instantly re-activate itself from the
      // shared bridge the moment its own seat is freed.
      await host.removeLicense();
      setLicense(null);
      setKeyInput("");
      setNote("License removed. Free tier (local dictation) is unaffected.");
    } catch (error) {
      setNote(
        `Couldn't remove license: ${words(error instanceof Error ? error.message : String(error))}`,
      );
    } finally {
      setBusy(false);
    }
  }

  async function onRequestWindowsLinkCode() {
    if (winLinkBusy) return;
    setWinLinkBusy(true);
    setWinLinkError("");
    try {
      const result = await host.requestWindowsLinkCode();
      setWinLinkCode(result);
    } catch (e) {
      setWinLinkError(e instanceof Error ? e.message : String(e));
    } finally {
      setWinLinkBusy(false);
    }
  }

  async function onRedeemWindowsLinkCode() {
    const trimmed = winRedeemCodeInput.trim();
    if (!trimmed || winRedeemBusy) return;
    setWinRedeemBusy(true);
    setWinRedeemError("");
    setWinRedeemNote("");
    try {
      await host.redeemWindowsLinkCode(trimmed);
      setWinRedeemCodeInput("");
      setWinRedeemNote("Pro activated on this PC.");
    } catch (e) {
      // A rejected redeem (expired, already used, or wrong code) must not
      // read as activated — the input stays so the user can retry with a
      // fresh code from the Apple device.
      setWinRedeemError(e instanceof Error ? e.message : String(e));
    } finally {
      setWinRedeemBusy(false);
    }
  }

  const body = (
    <>
      <Row label="Plan" settingId="about.plan">
        <span className="font-semibold text-[var(--paper)]">{pro ? "Pro active" : "Free"}</span>
      </Row>

      {storageState.kind === "error" ? (
        <div
          role="alert"
          className="rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--danger),transparent_45%)] bg-[color-mix(in_srgb,var(--danger),transparent_90%)] p-[var(--space-3)]"
        >
          <p className="m-0 text-[length:var(--size-sm)]">
            Secure license storage is unavailable. {appName} will stay Free until your OS
            credential store is unlocked or repaired; license credentials were not saved to
            browser storage.
          </p>
          <button
            type="button"
            className={cx(ui.button, ui.buttonGhost, "mt-[var(--space-2)]")}
            disabled={busy}
            onClick={() => void onRetryStorage()}
          >
            Retry secure storage
          </button>
        </div>
      ) : null}

      {!pro ? (
        <>
          <Row label="License key" settingId="about.license-key">
            <div className="flex items-center gap-2">
              <input
                className={cx(ui.input, "flex-1")}
                type="text"
                inputMode="text"
                autoComplete="off"
                spellCheck={false}
                placeholder={host.keyPlaceholder}
                aria-label="License key"
                maxLength={80}
                value={keyInput}
                onChange={(e) => setKeyInput(host.formatLicenseKey(e.target.value))}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void onActivate();
                }}
                disabled={busy}
              />
              <button
                type="button"
                className={ui.button}
                onClick={() => void onActivate()}
                disabled={busy || !keyInput.trim()}
              >
                {busy ? "Activating…" : "Activate"}
              </button>
            </div>
          </Row>
          <Row
            label="Get Pro"
            sublabel={`Try every Pro feature for ${trialDays} days, or buy a lifetime license`}
            settingId="about.get-pro"
          >
            <div className="flex flex-col items-end gap-1">
              <button
                type="button"
                className={cx(ui.button, ui.buttonSecondary)}
                onClick={() => setTrialOpen(true)}
              >
                Start free trial
              </button>
              <span className="text-[length:var(--size-xs)] text-[var(--muted)]">
                No card or payment information
              </span>
            </div>
          </Row>

          {/* HR-PRO-015: redeem side. Windows never sees a raw StoreKit JWS
              or iCloud secret — only this short-lived single-use code
              generated on an already-leased Apple device (see the Mac panel
              below). */}
          {isWindows && (
            <Row
              label="Link from a Mac or iPhone"
              sublabel="On your already-activated Mac or iPhone: License → Link a Windows PC → generate a code. Codes expire in minutes and work once."
              settingId="about.link-from-a-mac-or-iphone"
            >
              <div className="flex flex-col items-end gap-2">
                <div className="flex items-center gap-2">
                  <input
                    className={cx(ui.input, "flex-1")}
                    type="text"
                    inputMode="text"
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="Code from Mac/iPhone"
                    aria-label="Windows link code"
                    value={winRedeemCodeInput}
                    onChange={(e) => setWinRedeemCodeInput(e.target.value.toUpperCase())}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") void onRedeemWindowsLinkCode();
                    }}
                    disabled={winRedeemBusy}
                  />
                  <button
                    type="button"
                    className={ui.button}
                    onClick={() => void onRedeemWindowsLinkCode()}
                    disabled={winRedeemBusy || !winRedeemCodeInput.trim()}
                  >
                    {winRedeemBusy ? "Linking…" : "Activate"}
                  </button>
                </div>
                <ErrorBanner message={winRedeemError} onDismiss={() => setWinRedeemError("")} />
                {winRedeemNote && <span className={ui.keyHint}>{winRedeemNote}</span>}
              </div>
            </Row>
          )}
        </>
      ) : (
        <>
          {isMac && (
            <Row
              label="Link a Windows PC"
              sublabel={`Generates a one-time code, valid a few minutes, to type into ${appName} on Windows. Never shares your raw key, App Store receipt, or iCloud secret.`}
              settingId="about.link-a-windows-pc"
            >
              <div className="flex flex-col items-end gap-2">
                {!winLinkCode ? (
                  <div className="flex items-center gap-2">
                    <button
                      type="button"
                      className={ui.button}
                      onClick={() => void onRequestWindowsLinkCode()}
                      disabled={winLinkBusy}
                    >
                      {winLinkBusy ? "Generating…" : "Generate code"}
                    </button>
                  </div>
                ) : (
                  <div className="flex flex-col items-end gap-1">
                    <span className={cx(ui.keyHint, "text-[length:var(--size-md)]")}>{winLinkCode.code}</span>
                    <span className={ui.mutedXs}>
                      Single use · expires {new Date(winLinkCode.expiresAt).toLocaleTimeString()}
                    </span>
                    <button
                      type="button"
                      className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
                      onClick={() => {
                        setWinLinkCode(null);
                      }}
                    >
                      Generate a new code
                    </button>
                  </div>
                )}
                <ErrorBanner message={winLinkError} onDismiss={() => setWinLinkError("")} />
              </div>
            </Row>
          )}
          <Row label="Manage" settingId="about.manage">
            <button
              type="button"
              className={cx(ui.button, ui.buttonGhost)}
              onClick={() => void onRemove()}
              disabled={busy}
            >
              Remove license
            </button>
          </Row>
        </>
      )}

      {note && (
        <p
          data-license-note
          className={cx(ui.fieldNote, ui.fieldNoteGap)}
          aria-live="polite"
        >
          {note}
        </p>
      )}

      {trialOpen && (
        /* Scrim: click-outside OR Escape (bubbling from the dialog's fields)
           closes. The dialog child carries the semantics; keyboard users have
           Escape plus the Close/Cancel buttons, so the scrim itself stays
           non-semantic — the standard modal-backdrop pattern. */
        // biome-ignore lint/a11y/noStaticElementInteractions: modal backdrop; dialog child owns semantics, Escape + buttons cover keyboard.
        <div
          role="presentation"
          className="fixed inset-0 z-50 grid place-items-center bg-[rgba(0,0,0,0.42)] p-[var(--space-4)] [backdrop-filter:blur(4px)]"
          onClick={(e) => {
            if (e.target === e.currentTarget) setTrialOpen(false);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") setTrialOpen(false);
          }}
        >
          <div
            role="dialog"
            aria-modal="true"
            aria-labelledby="trial-title"
            className="w-full max-w-[400px] rounded-[var(--radius-md)] border border-[var(--glass-edge)] bg-[var(--surface-1)] p-[var(--space-5)] text-[var(--paper)] [box-shadow:0_24px_70px_rgba(0,0,0,0.45),inset_0_1px_0_var(--glass-highlight)]"
          >
            <div className="flex items-start justify-between gap-[var(--space-3)]">
              <h3 id="trial-title" className="m-0 [font-family:var(--font-display)] text-[length:var(--size-lg)]">
                Start your free trial
              </h3>
              <button
                type="button"
                aria-label="Close"
                className="shrink-0 rounded-[var(--radius-sm)] border-0 bg-transparent p-[2px] text-[length:var(--size-lg)] leading-none text-[var(--muted)] hover:text-[var(--paper)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
                onClick={() => setTrialOpen(false)}
              >
                ✕
              </button>
            </div>
            <p className="m-0 mt-[var(--space-2)] text-[length:var(--size-sm)] leading-[1.5] text-[var(--muted)]">
              {trialDays} days, every Pro feature, no card. We email your key — paste it above and hit Activate.
            </p>
            <input
              type="email"
              autoComplete="email"
              placeholder="you@example.com"
              aria-label="Email for trial key"
              className={cx(ui.input, "mt-[var(--space-4)] w-full")}
              value={trialEmail}
              onChange={(e) => setTrialEmail(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void onRequestTrial();
              }}
            />
            {trialNote && (
              <p className="m-0 mt-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--muted)]" aria-live="polite">
                {trialNote}
              </p>
            )}
            <div className="mt-[var(--space-4)] flex justify-end gap-[var(--space-2)]">
              <button
                type="button"
                className={cx(ui.button, ui.buttonGhost)}
                onClick={() => setTrialOpen(false)}
              >
                Cancel
              </button>
              <button
                type="button"
                className={ui.button}
                disabled={trialBusy || !trialEmail.trim()}
                onClick={() => void onRequestTrial()}
              >
                {trialBusy ? "Sending…" : "Email me a key"}
              </button>
            </div>
            <p className="m-0 mt-[var(--space-3)] text-center text-[length:var(--size-xs)] text-[var(--muted-2)]">
              Same flow as first-run onboarding — you never leave the app.
            </p>
          </div>
        </div>
      )}
    </>
  );

  if (bare) return body;
  return (
    <SettingsGroup title="License">
      {body}
      <p className="m-0 mx-[14px] mt-[7px] text-[length:var(--size-xs)] leading-[1.45] text-[var(--muted)] [text-wrap:pretty]">
        {`Local dictation, inline formatting and zephyr stop/send/cancel are free. Pro adds longer files, more languages, app and system commands${isMac ? " and Shortcuts" : ""}, and AI polish.`}
      </p>
    </SettingsGroup>
  );
}

export const LicenseSettings = memo(LicenseSettingsImpl);
