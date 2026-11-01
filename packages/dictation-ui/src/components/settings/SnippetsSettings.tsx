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
  snippets: Record<string, string>;
  patch: PatchFn;
}

const UNDO_WINDOW_MS = 6000;

// Find-replace text expansion. Stored in settings.snippets (object); the backend
// `apply_settings_patch` accepts a "snippets" object. trigger → replacement.
//
// Hub redesign: the Snippets tab of the Dictation page — add row, one-line note,
// then one surface with a hairline row per snippet.
function SnippetsSettings({ loaded, snippets, patch }: Props) {
  const [trigger, setTrigger] = useState("");
  const [replacement, setReplacement] = useState("");
  const [pendingRemoval, setPendingRemoval] = useState<string | null>(null);
  const [focusAfterRemoval, setFocusAfterRemoval] = useState<{ key: string; index: number } | null>(
    null,
  );
  // Snapshot of the just-removed entry, kept for the undo grace window. The
  // removal itself already committed (settings.patch is a full-object
  // overwrite, so "undo" is just patching the value back in - no separate
  // backend delete to hold off on).
  const [undo, setUndo] = useState<{ key: string; value: string } | null>(null);
  const undoTimerRef = useRef<number | undefined>(undefined);
  const confirmDialogRef = useRef<HTMLDialogElement>(null);
  const triggerInputRef = useRef<HTMLInputElement>(null);
  const entries = Object.entries(snippets ?? {});

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
    const removeButtons = document.querySelectorAll<HTMLButtonElement>("[data-snippet-remove]");
    const nextButton =
      removeButtons.length > 0
        ? removeButtons.item(Math.min(focusAfterRemoval.index, removeButtons.length - 1))
        : null;
    if (nextButton) nextButton.focus();
    else triggerInputRef.current?.focus();
    setFocusAfterRemoval(null);
  }, [entries, focusAfterRemoval]);

  function save(next: Record<string, string>) {
    void patch("snippets", next);
  }
  function add() {
    const t = trigger.trim();
    if (t.length === 0 || replacement.trim().length === 0) return;
    save({ ...(snippets ?? {}), [t]: replacement.trim() });
    setTrigger("");
    setReplacement("");
  }
  function requestRemove(key: string) {
    setPendingRemoval(key);
  }
  function confirmRemove() {
    const key = pendingRemoval;
    setPendingRemoval(null);
    if (key === null) return;
    const value = snippets?.[key];
    const next = { ...(snippets ?? {}) };
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
    save({ ...(snippets ?? {}), [undo.key]: undo.value });
    setUndo(null);
  }

  return (
    <>
      <DictationPanel
        form={
          <div className={dictationFormColumns}>
            <input
              ref={triggerInputRef}
              type="text"
              aria-label="Snippet trigger"
              placeholder="Trigger"
              value={trigger}
              onChange={(e) => setTrigger(e.target.value)}
              className={dictationInput}
              disabled={!loaded}
              maxLength={100}
            />
            <input
              type="text"
              aria-label="Snippet replacement"
              placeholder="Expands to"
              value={replacement}
              onChange={(e) => setReplacement(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") add();
              }}
              className={dictationInput}
              disabled={!loaded}
              maxLength={500}
            />
            <button
              type="button"
              className={dictationAddButton}
              onClick={add}
              disabled={!loaded || trigger.trim().length === 0 || replacement.trim().length === 0}
            >
              <Plus aria-hidden="true" className="h-4 w-4" />
              Add
            </button>
          </div>
        }
        note="Expands after delivery."
      >
        {entries.length === 0 ? (
          <div className={dictationEmpty}>No snippets yet. Add one above.</div>
        ) : (
          entries.map(([k, v]) => (
            <div key={k} className={dictationRow}>
              <span className="min-w-0 justify-self-start truncate rounded-[6px] bg-[var(--surface-3)] px-[8px] py-[2px] [font-family:var(--font-mono)] text-[length:var(--size-xs)] font-medium text-[var(--paper)]">
                {k}
              </span>
              <span className="flex min-w-0 items-center gap-[var(--space-2)] max-[720px]:hidden">
                <span aria-hidden="true" className="shrink-0 text-[var(--muted-2)]">
                  →
                </span>
                <span className="min-w-0 truncate text-[var(--muted)]">{v}</span>
              </span>
              <button
                type="button"
                data-snippet-remove={k}
                className={dictationRowRemove}
                onClick={() => requestRemove(k)}
                disabled={!loaded}
                aria-label={`Remove snippet ${k}`}
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
          aria-labelledby="snippet-remove-title"
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
            id="snippet-remove-title"
            className="m-0 [font-family:var(--font-display)] text-[length:var(--size-xl)] text-[var(--paper)]"
          >
            Remove this snippet?
          </h2>
          <p className="m-0 mt-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--muted)]">
            &ldquo;{pendingRemoval}&rdquo; will stop expanding on delivery. You can undo this right
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

export default memo(SnippetsSettings);
