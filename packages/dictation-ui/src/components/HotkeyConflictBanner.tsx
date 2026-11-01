import { useEffect, useState } from "react";
import { useTauriEvent } from "../hooks/useTauriEvent";
import { invoke } from "../lib/tauri";
import { cx, ui } from "../lib/ui";
import { hostAppName } from "../appName";

type HotkeyConflict = {
  shortcuts: string[];
  message: string;
};

export default function HotkeyConflictBanner() {
  const [conflict, setConflict] = useState<HotkeyConflict | null>(null);
  const [retrying, setRetrying] = useState(false);

  useEffect(() => {
    void invoke<HotkeyConflict | null>("get_hotkey_conflict")
      .then(setConflict)
      .catch(() => undefined);
  }, []);

  useTauriEvent<HotkeyConflict | null>("hotkey-registration-status", setConflict);

  async function retry() {
    setRetrying(true);
    try {
      await invoke("register_hotkeys_now");
    } catch {
      const current = await invoke<HotkeyConflict | null>("get_hotkey_conflict").catch(
        () => conflict,
      );
      setConflict(current);
    } finally {
      setRetrying(false);
    }
  }

  if (!conflict) return null;

  return (
    <aside
      role="alert"
      aria-live="assertive"
      className="fixed bottom-4 left-1/2 z-[70] flex w-[min(680px,calc(100vw-32px))] -translate-x-1/2 items-center gap-4 rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--danger),transparent_35%)] bg-[var(--surface-2)] px-4 py-3 text-[length:var(--size-sm)] text-[var(--paper)] [box-shadow:0_18px_50px_rgba(0,0,0,.45)]"
    >
      <div className="min-w-0 flex-1">
        <strong>Keyboard shortcuts are unavailable.</strong> {hostAppName()} could not register{" "}
        {conflict.shortcuts.join(", ")} because another app is using
        {conflict.shortcuts.length === 1 ? " it" : " them"}. Change or close that app, then Retry.
      </div>
      <button
        type="button"
        className={cx(ui.button, ui.buttonCompact)}
        disabled={retrying}
        onClick={() => void retry()}
      >
        <span className={retrying ? "hidden" : undefined}>Retry</span>
        {retrying ? <span>Retrying…</span> : null}
      </button>
    </aside>
  );
}
