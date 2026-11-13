// Device management — HR-ACCOUNT-P6, HR-PRO-011/012 (docs/DOGFOOD.md).
// Lists every device leased to the signed-in account: platform,
// friendly label, last seen, and an honest authorization state (active,
// renewing, offline grace, removed, revoked) — never collapsing offline
// grace into looking identical to fully active (HR-PRO-012). Rename and
// remove are each their own explicit action; removal is destructive and
// requires confirmation, and a failed removal renders as FAILED, never as a
// silently-successful UI update (the same standard HR-HISTORY-011A sets for
// History's destructive actions).
//
// ONE "Remove" action covers HR-ACCOUNT-P6's remove, reassign, AND
// lost-device recovery — there is deliberately no separate reassign button.
// `removeAccountDevice` (lib/license.ts) frees the device's pool slot
// transactionally (HR-PRO-009); freeing the slot IS lost-device recovery and
// the precondition for reassignment (a replacement device then activates
// into the freed slot). An earlier version of this component shipped a
// second "Reassign" button calling a since-removed `account_device_reassign`
// command that was a literal alias for remove — the account-session lane
// removed that command as a defect (a command whose name promises a
// different operation than it performs) and this component was corrected to
// match: one honest action, whose confirmation copy says it frees the seat.
//
// Requires a signed-in account (HR-ACCOUNT-P5) — this section renders
// nothing while signed out, so it never implies device management is
// available to a Free/local-only or not-yet-linked user.

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { CompactSettingsGroup } from "../components/settings/CompactSettingsGroup.js";
import ErrorBanner from "../components/ErrorBanner.js";
import { hostAppName } from "../host.js";
import { cx, ui } from "../lib/ui.js";
import { sectionClient, type AccountDeviceRow } from "./client.js";

// Non-color status cue (HR-UI-001): the label text itself, not a bare color
// dot, is what distinguishes these — color is an additional hint layered on
// top, never the only signal. An unrecognized value renders literally
// rather than guessing, since neither this lane's contract nor the real
// Rust struct (account_session.rs's AccountDeviceRow) enumerates a closed
// set of `state` values.
function deviceStateLabel(state: string): { text: string; tone: "ok" | "warn" | "muted" | "danger" } {
  switch (state) {
    case "active":
      return { text: "Active", tone: "ok" };
    case "renewing":
      return { text: "Renewing", tone: "warn" };
    case "offline_grace":
      return { text: "Offline grace", tone: "warn" };
    case "removed":
      return { text: "Removed", tone: "muted" };
    case "revoked":
      return { text: "Revoked", tone: "danger" };
    default:
      return { text: state || "Unknown", tone: "muted" };
  }
}

function toneClass(tone: "ok" | "warn" | "muted" | "danger") {
  switch (tone) {
    case "ok":
      return "text-[var(--ok)]";
    case "warn":
      return "text-[var(--warn)]";
    case "danger":
      return ui.dangerText;
    default:
      return "text-[var(--muted)]";
  }
}

/** `unixMs` is the native view's Unix-millisecond time (null when the service sent none). */
function formatLastSeen(unixMs: number | null): string {
  if (unixMs == null) return "never seen";
  try {
    return new Date(unixMs).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  } catch {
    return "never seen";
  }
}

function DeviceRow({
  device,
  onChanged,
}: {
  device: AccountDeviceRow;
  onChanged: () => void;
}) {
  const [renaming, setRenaming] = useState(false);
  const [label, setLabel] = useState(device.label ?? "");
  const [removeConfirming, setRemoveConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [rowError, setRowError] = useState("");
  const renameInputRef = useRef<HTMLInputElement>(null);
  const state = deviceStateLabel(device.state);
  const removed = device.state === "removed" || device.state === "revoked";

  useEffect(() => {
    if (renaming) renameInputRef.current?.focus();
  }, [renaming]);

  async function onRename() {
    const trimmed = label.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setRowError("");
    try {
      // `device.id`, not a hash — see AccountDeviceRow's doc comment in
      // lib/license.ts for why the command's parameter stays named
      // `deviceIdHash` even though this is what it actually expects.
      await sectionClient("account").renameDevice(device.id, trimmed);
      setRenaming(false);
      onChanged();
    } catch (e) {
      // Failure keeps the edit field open with the attempted value, rather
      // than silently reverting to the old label as if nothing happened.
      setRowError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onRemove() {
    if (busy) return;
    setBusy(true);
    setRowError("");
    try {
      await sectionClient("account").removeDevice(device.id);
      setRemoveConfirming(false);
      onChanged();
    } catch (e) {
      // A rejected remove call must render as a failure — the row must NOT
      // be optimistically marked removed here (HR-HISTORY-011A standard).
      setRowError(e instanceof Error ? e.message : String(e));
      setRemoveConfirming(false);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col items-stretch gap-2 px-[var(--space-4)] py-[var(--space-3)]">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          {!renaming ? (
            <p className="m-0 text-[var(--paper)]">{device.label || "Unnamed device"}</p>
          ) : (
            <div className="flex items-center gap-2">
              <input
                ref={renameInputRef}
                className={cx(ui.input, "min-w-[160px]")}
                value={label}
                onChange={(e) => setLabel(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void onRename();
                  if (e.key === "Escape") {
                    setRenaming(false);
                    setLabel(device.label ?? "");
                  }
                }}
                disabled={busy}
                aria-label="Device name"
              />
              <button type="button" className={cx(ui.button, ui.buttonCompact)} onClick={() => void onRename()} disabled={busy || !label.trim()}>
                Save
              </button>
              <button
                type="button"
                className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
                onClick={() => {
                  setRenaming(false);
                  setLabel(device.label ?? "");
                }}
                disabled={busy}
              >
                Cancel
              </button>
            </div>
          )}
          <p className={ui.historyMeta}>
            {device.platform ?? "unknown platform"} · last seen{" "}
            {formatLastSeen(device.lastSeenAt)}
          </p>
        </div>
        <span className={cx("shrink-0 text-[length:var(--size-sm)] font-semibold", toneClass(state.tone))}>
          {state.text}
        </span>
      </div>

      {!removed && (
        <div className="flex flex-wrap items-center gap-2">
          {!renaming && (
            <button type="button" className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)} onClick={() => setRenaming(true)} disabled={busy}>
              Rename
            </button>
          )}

          {!removeConfirming ? (
            <button
              type="button"
              className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
              onClick={() => setRemoveConfirming(true)}
              disabled={busy}
            >
              Remove…
            </button>
          ) : (
            <>
              <span className={ui.mutedXs}>
                Removes this device from your license. You can then activate another device in its
                place. Not undoable from here.
              </span>
              <button
                type="button"
                className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
                onClick={() => setRemoveConfirming(false)}
                disabled={busy}
              >
                Cancel
              </button>
              <button type="button" className={cx(ui.button, ui.buttonDanger, ui.buttonCompact)} onClick={() => void onRemove()} disabled={busy}>
                {busy ? "Removing…" : "Confirm remove"}
              </button>
            </>
          )}
        </div>
      )}

      {rowError && (
        <p className={cx(ui.fieldNote, ui.dangerText)} role="alert">
          {rowError}
        </p>
      )}
    </div>
  );
}

function DeviceManagementSettingsImpl() {
  const [signedIn, setSignedIn] = useState<boolean | null>(null);
  const [devices, setDevices] = useState<AccountDeviceRow[] | null>(null);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    try {
      const status = await sectionClient("account").sessionStatus();
      setSignedIn(status.signedIn);
      if (!status.signedIn) {
        setDevices(null);
        return;
      }
      const list = await sectionClient("account").devices();
      setDevices(list);
      setError("");
    } catch (e) {
      // Unknown, not "no devices" — an empty list here would read as "you
      // have no devices," which is a false statement if the call merely
      // failed to reach the server.
      setError(e instanceof Error ? e.message : String(e));
      setDevices(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (signedIn !== true) return null;

  return (
    <CompactSettingsGroup
      title="Devices"
      footer={`Every device on your ${hostAppName()} account. Offline grace means a device stays authorized for a short time while it cannot reach the server.`}
      settingId="about.devices"
    >
      <ErrorBanner message={error} onDismiss={() => setError("")} />
      {devices === null && !error ? (
        <p className={cx(ui.mutedSm, "m-0 px-[var(--space-4)] py-[var(--space-3)]")}>
          Loading devices…
        </p>
      ) : devices && devices.length === 0 ? (
        <p className={cx(ui.mutedSm, "m-0 px-[var(--space-4)] py-[var(--space-3)]")}>
          No devices on this account yet.
        </p>
      ) : (
        devices?.map((d) => (
          <DeviceRow key={d.id} device={d} onChanged={() => void refresh()} />
        ))
      )}
    </CompactSettingsGroup>
  );
}

export const DeviceManagementSettings = memo(DeviceManagementSettingsImpl);
