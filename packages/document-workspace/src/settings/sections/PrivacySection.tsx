import { useState } from "react";
import { toast } from "@rightkit/platform-ui/headless";
import { drainSessionSaves } from "../../app/useDocumentRuntimeEffects.js";
import { ActionDialog, SegmentedControl } from "@rightkit/react-shell/react";
import { SettingsGroup } from "@rightkit/settings-ui";
import { DIALOG_SLOTS, cancelAction, dangerAction } from "@rightkit/document-core/app/chrome";
import { purgeUserPaths } from "@rightkit/document-core/state/tauriBridge";
import { clearAllDiagnostics, collectDiagnosticsText } from "@rightkit/document-core/state/diagnostics";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { onOff } from "./onOff.js";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../../appName.js";

export function PrivacySection({ onHistoryPurged }: { onHistoryPurged?: () => void } = {}) {
  const { settings, update } = useSettings();
  const [cleared, setCleared] = useState(false);
  const [copied, setCopied] = useState(false);
  const [confirmPurge, setConfirmPurge] = useState(false);
  const [purging, setPurging] = useState(false);
  const runPurge = async () => {
    setPurging(true);
    try {
      await drainSessionSaves();
      await purgeUserPaths();
      onHistoryPurged?.();
      toast("History cleared", "success");
    } catch {
      toast("Couldn't clear history", "error");
    } finally {
      setPurging(false);
      setConfirmPurge(false);
    }
  };
  return (
    <>
      <SettingsGroup title="Session restore">
        <p className="mb-3 max-w-prose text-sm text-muted">
          Reopen file-backed tabs from the previous desktop session. Untitled scratch notes are
          still saved locally as drafts.
        </p>
        <SegmentedControl
          label="Session restore"
          options={onOff("Session restore")}
          value={settings.restoreSession ? "on" : "off"}
          onChange={(v) => update({ restoreSession: v === "on" })}
        />
      </SettingsGroup>
      <SettingsGroup title="Diagnostic log">
        <p className="mb-3 max-w-prose text-sm text-muted">
          {hostAppName()} keeps a small local log of loads, document timings, and errors to help diagnose
          a crash or stuck launch. It is stored only on this device and is never sent anywhere. You
          can turn it off, copy it, or clear what’s stored now.
        </p>
        <SegmentedControl
          label="Diagnostic log"
          options={onOff("Diagnostics")}
          value={settings.diagnostics ? "on" : "off"}
          onChange={(v) => update({ diagnostics: v === "on" })}
        />
      </SettingsGroup>
      <SettingsGroup title="Stored data">
        <button
          type="button"
          onClick={() => {
            void collectDiagnosticsText()
              .then((text) => writeClipboardText(text))
              .then(() => setCopied(true))
              .catch(() => {
                // Clipboard access is permission-gated; leaving the log on screen is sufficient.
              });
          }}
          className="vr-press vr-focus mr-2 rounded border border-line px-3 py-1.5 font-sans text-xs text-muted hover:border-copper hover:text-copper-hi"
        >
          Copy diagnostic log
        </button>
        <button
          type="button"
          onClick={() => {
            void clearAllDiagnostics()
              .then(() => setCleared(true))
              .catch(() => setCleared(false));
          }}
          className="vr-press vr-focus rounded border border-line px-3 py-1.5 font-sans text-xs text-muted hover:border-copper hover:text-copper-hi"
        >
          Clear diagnostic log
        </button>
        <button
          type="button"
          onClick={() => setConfirmPurge(true)}
          className="vr-press vr-focus ml-2 rounded border border-line px-3 py-1.5 font-sans text-xs text-muted hover:border-danger/60 hover:text-paper"
        >
          Clear all history
        </button>
        {copied && <span className="ml-3 font-mono text-xs text-copper-hi">copied</span>}
        {cleared && <span className="ml-3 font-mono text-xs text-copper-hi">cleared</span>}
      </SettingsGroup>
      {confirmPurge && (
        <ActionDialog
          title="Clear all history?"
          ariaLabel="Clear all history?"
          description="This removes recent files, the saved open-tab session and pinned files. Your settings are not changed."
          onClose={() => setConfirmPurge(false)}
          width={440}
          role="alertdialog"
          slots={DIALOG_SLOTS}
          actions={[
            cancelAction(() => setConfirmPurge(false)),
            { ...dangerAction("Clear history", () => void runPurge()), disabled: purging },
          ]}
        />
      )}
    </>
  );
}
