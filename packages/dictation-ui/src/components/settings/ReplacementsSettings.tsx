import { memo, useEffect, useRef, useState } from "react";
import { Plus, X } from "@phosphor-icons/react";
import type { PatchFn } from "../../types/settings";
import { cx, ui } from "../../lib/ui";
import {
  DictationPanel,
  dictationAddButton,
  dictationEmpty,
  dictationFormColumns,
  dictationInput,
  dictationRow,
  dictationRowRemove,
} from "./DictationSection";

interface Props {
  loaded: boolean;
  replacements: Record<string, string>;
  patch: PatchFn;
}

const MAX_KEY_LEN = 64;
const MAX_VALUE_LEN = 256;
const UNDO_WINDOW_MS = 6000;

// Deterministic text replacements. Stored in settings.replacements (object); the
// backend `apply_settings_patch` accepts a "replacements" object. from → to.
//
// Hub redesign: the Text replacements tab of the Dictation page — add row,
// one-line note (validation errors append to it), then one hairline-row surface.
function ReplacementsSettings({ loaded, replacements, patch }: Props) {
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [error, setError] = useState("");
  const [pendingRemoval, setPendingRemoval] = useState<string | null>(null);
  const [focusAfterRemoval, setFocusAfterRemoval] = useState<{ key: string; index: number } | null>(
    null,
  );
  // See SnippetsSettings.tsx: removal already committed by the time undo
  // shows (patch() overwrites the whole object), so undo is just patching
  // the snapshot back in - no delayed backend call to hold off on.
  const [undo, setUndo] = useState<{ key: string; value: string } | null>(null);
  const undoTimerRef = useRef<number | undefined>(undefined);
  const confirmDialogRef = useRef<HTMLDialogElement>(null);
  const fromInputRef = useRef<HTMLInputElement>(null);
  const entries = Object.entries(replacements ?? {});

  useEffect(() => {
    return () => {
      if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
    };
  }, []);

  useEffect(() => {
    const el = confirmDialogRef.current;
    if (!pendingRemoval) return;
    el?.showModal();
    return () => el?.close();
  }, [pendingRemoval]);

  useEffect(() => {
    if (!focusAfterRemoval || entries.some(([key]) => key === focusAfterRemoval.key)) return;
    const removeButtons = document.querySelectorAll<HTMLButtonElement>("[data-replacement-remove]");
    const nextButton =
      removeButtons.length > 0
        ? removeButtons.item(Math.min(focusAfterRemoval.index, removeButtons.length - 1))
        : null;
    if (nextButton) nextButton.focus();
    else fromInputRef.current?.focus();
    setFocusAfterRemoval(null);
  }, [entries, focusAfterRemoval]);

  function save(next: Record<string, string>) {
    void patch("replacements", next);
  }
  function add() {
    const key = from.trim();
    const value = to.trim();
    setError("");
    if (key.length === 0) {
      setError("Enter a phrase to replace.");
      return;
    }
    if (key.length > MAX_KEY_LEN) {
      setError(`Phrase must be ${MAX_KEY_LEN} characters or fewer.`);
      return;
    }
    if (value.length === 0) {
      setError("Enter the replacement text.");
      return;
    }
    if (value.length > MAX_VALUE_LEN) {
      setError(`Replacement must be ${MAX_VALUE_LEN} characters or fewer.`);
      return;
    }
    save({ ...(replacements ?? {}), [key]: value });
    setFrom("");
    setTo("");
  }
  function requestRemove(key: string) {
    setPendingRemoval(key);
  }
  function confirmRemove() {
    const key = pendingRemoval;
    setPendingRemoval(null);
    if (key === null) return;
    const value = replacements?.[key];
    const next = { ...(replacements ?? {}) };
    delete next[key];
    setFocusAfterRemoval({
      key,
      index: Math.max(
        0,
        entries.findIndex(([entryKey]) => entryKey === key),
      ),
    });
    save(next);
    if (value === undefined) return;
    if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
    setUndo({ key, value });
    undoTimerRef.current = window.setTimeout(() => {
      undoTimerRef.current = undefined;
      setUndo(null);
    }, UNDO_WINDOW_MS);
  }
  function undoRemove() {
    if (!undo) return;
    if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
    save({ ...(replacements ?? {}), [undo.key]: undo.value });
    setUndo(null);
  }

  return (
    <>
      <DictationPanel
        form={
          <div className={dictationFormColumns}>
            <input
              ref={fromInputRef}
              type="text"
              aria-label="Replacement phrase"
              placeholder="Heard as"
              value={from}
              onChange={(e) => setFrom(e.target.value)}
              className={dictationInput}
              disabled={!loaded}
              maxLength={MAX_KEY_LEN}
            />
            <input
              type="text"
              aria-label="Replacement text"
              placeholder="Replace with"
              value={to}
              onChange={(e) => setTo(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") add();
              }}
              className={dictationInput}
              disabled={!loaded}
              maxLength={MAX_VALUE_LEN}
            />
            <button
              type="button"
              className={dictationAddButton}
              onClick={add}
              disabled={!loaded || from.trim().length === 0 || to.trim().length === 0}
            >
              <Plus aria-hidden="true" className="h-4 w-4" />
              Add
            </button>
          </div>
        }
        note={
          <>
            Applied on-device before anything else.
            {error ? (
              <span className="ml-[var(--space-2)] text-[var(--danger)]">{error}</span>
            ) : null}
          </>
        }
      >
        {entries.length === 0 ? (
          <div className={dictationEmpty}>No replacements yet. Add one above.</div>
        ) : (
          entries.map(([k, v]) => (
            <div key={k} className={dictationRow}>
              <span className="min-w-0 truncate [font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--paper)]">
                {k}
              </span>
              <span className="flex min-w-0 items-center gap-[var(--space-2)] max-[720px]:hidden">
                <span aria-hidden="true" className="shrink-0 text-[var(--muted-2)]">
                  →
                </span>
                <span className="min-w-0 truncate [font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--paper)]">
                  {v}
                </span>
              </span>
              <button
                type="button"
                data-replacement-remove={k}
                className={dictationRowRemove}
                onClick={() => requestRemove(k)}
                disabled={!loaded}
                aria-label={`Remove replacement ${k}`}
                title="Remove"
              >
                <X aria-hidden="true" className="h-3.5 w-3.5" weight="bold" />
              </button>
            </div>
          ))
        )}
      </DictationPanel>

      {pendingRemoval !== null ? (
        <dialog
          ref={confirmDialogRef}
          aria-labelledby="replacement-remove-title"
          className="m-auto w-full max-w-[420px] rounded-[var(--radius-md)] border border-[var(--glass-edge)] bg-[var(--surface-1)] p-[var(--space-5)] text-[var(--paper)] [box-shadow:0_20px_70px_rgba(0,0,0,0.42),inset_0_1px_0_var(--glass-highlight)] backdrop:bg-[rgba(0,0,0,0.38)] backdrop:[backdrop-filter:blur(10px)]"
          onCancel={(e) => {
            e.preventDefault();
            setPendingRemoval(null);
          }}
          onClick={(e) => {
            if (e.target === confirmDialogRef.current) setPendingRemoval(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              setPendingRemoval(null);
            }
          }}
        >
          <h2
            id="replacement-remove-title"
            className="m-0 [font-family:var(--font-display)] text-[length:var(--size-xl)] text-[var(--paper)]"
          >
            Remove this replacement?
          </h2>
          <p className="m-0 mt-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--muted)]">
            &ldquo;{pendingRemoval}&rdquo; will stop being auto-corrected. You can undo this right
            after removing it.
          </p>
          <div className="mt-[var(--space-5)] flex justify-end gap-[var(--space-2)]">
            <button
              type="button"
              className={cx(ui.button, ui.buttonGhost)}
              onClick={() => setPendingRemoval(null)}
            >
              Cancel
            </button>
            <button
              type="button"
              className={cx(ui.button, ui.buttonDanger)}
              onClick={confirmRemove}
            >
              Remove
            </button>
          </div>
        </dialog>
      ) : null}

      {undo ? (
        <output
          aria-live="polite"
          className="fixed bottom-[var(--space-5)] left-1/2 z-40 flex -translate-x-1/2 items-center gap-[var(--space-3)] rounded-[var(--radius-sm)] border border-[var(--glass-edge)] bg-[var(--surface-2)] px-[var(--space-3)] py-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--paper)] [box-shadow:var(--surface-shadow-hover)]"
        >
          Removed &ldquo;{undo.key}&rdquo;.
          <button
            type="button"
            className="cursor-pointer rounded-[var(--radius-sm)] border-0 bg-transparent p-0 font-semibold text-[var(--accent-cool)] underline underline-offset-2 focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
            onClick={undoRemove}
          >
            Undo
          </button>
        </output>
      ) : null}
    </>
  );
}

export default memo(ReplacementsSettings);
