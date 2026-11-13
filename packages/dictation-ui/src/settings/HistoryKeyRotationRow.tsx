import { useState } from "react";
import { CompactSettingRow } from "../components/settings/CompactSettingsGroup.js";
import { cx, ui } from "../lib/ui.js";
import { sectionClient } from "./client.js";

/** Settings action: rotate the key that encrypts local history. */
export function HistoryKeyRotationRow() {
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);

  async function rotate() {
    try {
      const ok = await sectionClient("app").confirm(
        "Re-encrypt your local history with a new key? Your history is kept. If anything fails, the current key stays in place.",
        { title: "Rotate encryption key", kind: "warning" },
      );
      if (!ok) return;
    } catch (e) {
      setResult({ ok: false, text: `Couldn't open the confirmation: ${String(e)}` });
      return;
    }
    setBusy(true);
    setResult(null);
    try {
      await sectionClient("app").rotateHistoryKey();
      setResult({ ok: true, text: "Encryption key rotated." });
    } catch (e) {
      setResult({ ok: false, text: `Rotation failed: ${String(e)}` });
    } finally {
      setBusy(false);
    }
  }

  return (
    <CompactSettingRow
      label="History encryption key"
      sublabel="History is encrypted on this device. Rotating re-encrypts it with a new key."
      settingId="settings.rotate-history-key"
    >
      <div className="flex items-center gap-3">
        {result ? (
          <span role="status" className="text-[length:var(--size-sm)]">
            {result.text}
          </span>
        ) : null}
        <button type="button" className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact)} disabled={busy} onClick={() => void rotate()}>
          {busy ? "Rotating…" : "Rotate encryption key"}
        </button>
      </div>
    </CompactSettingRow>
  );
}
