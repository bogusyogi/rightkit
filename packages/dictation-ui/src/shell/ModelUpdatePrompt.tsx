import { useEffect, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { hostAppName, hostShell } from "../host.js";
import { useTauriEvent } from "./useTauriEvent.js";
import {
  dismissModelUpdate,
  formatDownloadSize,
  hideModelUpdate,
  modelUpdatePhaseLabel,
  openModelUpdatePrompt,
  startModelUpdateDownload,
  useModelUpdate,
} from "../settings/modelUpdate.js";
import { cx, ui } from "../lib/ui.js";

type AvailablePayload = {
  backend?: string;
  model_label?: string;
  download_bytes?: number;
  reason?: string;
};

const bodyClass =
  "mb-0 mt-[var(--space-3)] text-[length:var(--size-sm)] leading-relaxed text-[var(--muted)]";

// Mounted once at the Hub root. Opens on `model-update://available`, on the
// one-shot native flag, or on the hub navigation request, from any tab. A
// native <dialog> opened with showModal() owns the focus trap and Escape.
export default function ModelUpdatePrompt() {
  const s = useModelUpdate();
  const dialogRef = useRef<HTMLDialogElement>(null);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const open = s.status !== "closed";

  useTauriEvent<AvailablePayload>("model-update://available", (p) => {
    if (p?.backend && p.backend !== hostShell().modelBackend) return;
    openModelUpdatePrompt({ modelLabel: p?.model_label, downloadBytes: p?.download_bytes });
  });
  useTauriEvent<{ download_model?: string }>("hub://navigate", (p) => {
    if (p?.download_model === hostShell().modelBackend) openModelUpdatePrompt();
  });

  // The one-shot flag survives a Hub webview created after the backend set it.
  // It opens the prompt; it never starts the download.
  useEffect(() => {
    void invoke<boolean>("take_retired_model_download_request")
      .then((requested) => {
        if (requested) openModelUpdatePrompt();
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    if (open && !el.open) {
      if (!opener.current && document.activeElement instanceof HTMLElement) {
        opener.current = document.activeElement;
      }
      el.showModal();
    } else if (!open && el.open) {
      el.close();
      if (opener.current?.isConnected) opener.current.focus();
      opener.current = null;
    }
  }, [open]);

  // Land focus on the primary action whenever the visible state changes.
  // biome-ignore lint/correctness/useExhaustiveDependencies: s.status re-runs the focus when the prompt changes state
  useEffect(() => {
    if (open) primaryRef.current?.focus();
  }, [open, s.status]);

  const size = formatDownloadSize(s.downloadBytes);
  const label = s.modelLabel;

  let title = "A faster speech model is ready";
  if (s.status === "downloading") title = `Updating to ${label}`;
  if (s.status === "done") title = "Update complete";
  if (s.status === "error") title = "The update did not finish";

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="model-update-title"
      aria-describedby="model-update-body"
      className="fixed inset-0 m-auto h-fit w-[min(460px,calc(100vw-32px))] rounded-[var(--radius-lg)] border border-[var(--glass-edge)] bg-[var(--surface-1)] p-[var(--space-5)] text-[var(--paper)] [box-shadow:0_24px_80px_rgba(0,0,0,0.45),inset_0_1px_0_var(--glass-highlight)] backdrop:bg-[rgba(0,0,0,0.48)] backdrop:[backdrop-filter:blur(10px)]"
      onCancel={(e) => {
        // Escape = Later while undecided; otherwise it only hides the sheet.
        e.preventDefault();
        if (s.status === "prompt") dismissModelUpdate();
        else hideModelUpdate();
      }}
    >
      {open ? (
        <>
          <h2
            id="model-update-title"
            className="m-0 [font-family:var(--font-display)] text-[length:var(--size-xl)] leading-tight"
          >
            {title}
          </h2>

          {s.status === "prompt" ? (
            <>
              <p id="model-update-body" className={bodyClass}>
                {label} is faster and smaller, with the same or better accuracy. Your current
                model keeps working until the new one is verified.
              </p>
              {size ? (
                <p className="mb-0 mt-[var(--space-2)] text-[length:var(--size-xs)] text-[var(--muted)]">
                  Download size: {size}. Runs on your device.
                </p>
              ) : null}
              <div className="mt-[var(--space-5)] flex flex-wrap justify-end gap-2">
                <button
                  type="button"
                  className={cx(ui.button, ui.buttonGhost)}
                  onClick={dismissModelUpdate}
                >
                  Later
                </button>
                <button
                  ref={primaryRef}
                  type="button"
                  className={ui.button}
                  onClick={() => void startModelUpdateDownload()}
                >
                  Download update
                </button>
              </div>
            </>
          ) : null}

          {s.status === "downloading" ? (
            <>
              <p id="model-update-body" className={bodyClass}>
                Your current model keeps working until this one is verified.
              </p>
              <div
                role="progressbar"
                aria-label={`${label} update progress`}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={s.phase === "starting" ? undefined : s.pct}
                className="mt-[var(--space-4)] h-2 w-full overflow-hidden rounded-full bg-[var(--surface-3)]"
              >
                <div
                  className="h-full rounded-full bg-[var(--accent)] transition-[width] duration-200 ease-[var(--ease-out-strong)]"
                  style={{ width: `${s.phase === "starting" ? 0 : s.pct}%` }}
                />
              </div>
              <p
                aria-live="polite"
                className="mb-0 mt-[var(--space-2)] text-[length:var(--size-xs)] text-[var(--muted)]"
              >
                {modelUpdatePhaseLabel(s)}
              </p>
              <div className="mt-[var(--space-5)] flex justify-end">
                <button
                  ref={primaryRef}
                  type="button"
                  className={cx(ui.button, ui.buttonGhost)}
                  onClick={hideModelUpdate}
                >
                  Hide
                </button>
              </div>
            </>
          ) : null}

          {s.status === "done" ? (
            <>
              <p id="model-update-body" className={bodyClass}>
                Ready. {hostAppName()} is now using {label}.
              </p>
              <div className="mt-[var(--space-5)] flex justify-end">
                <button
                  ref={primaryRef}
                  type="button"
                  className={ui.button}
                  onClick={hideModelUpdate}
                >
                  Done
                </button>
              </div>
            </>
          ) : null}

          {s.status === "error" ? (
            <>
              <p id="model-update-body" className={bodyClass}>
                Your current model is untouched. The app reported:
              </p>
              <p
                role="alert"
                className="mb-0 mt-[var(--space-2)] max-h-32 overflow-auto break-words rounded-[var(--radius-sm)] border border-[var(--line)] bg-[var(--surface-2)] px-[var(--space-3)] py-[var(--space-2)] [font-family:var(--font-mono)] text-[length:var(--size-xs)] leading-snug text-[var(--paper)]"
              >
                {s.error}
              </p>
              <div className="mt-[var(--space-5)] flex flex-wrap justify-end gap-2">
                <button
                  type="button"
                  className={cx(ui.button, ui.buttonGhost)}
                  onClick={hideModelUpdate}
                >
                  Close
                </button>
                <button
                  ref={primaryRef}
                  type="button"
                  className={ui.button}
                  onClick={() => void startModelUpdateDownload()}
                >
                  Try again
                </button>
              </div>
            </>
          ) : null}
        </>
      ) : null}
    </dialog>
  );
}
