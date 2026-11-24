// A settings list of key -> value pairs stored as one object (text replacements, snippets): add with
// validation, remove with confirmation, undo for a few seconds, focus after removal. The host supplies the save
// function, the validation messages and every label; the markup is the host's. Imports only `react`.
import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { focusRemoveButton, useRemovalFlow } from "./removal.js";

export interface UseKeyValueListOptions {
  /** The stored object. */
  value: Record<string, string> | undefined;
  /** Stores the whole next object. */
  save: (next: Record<string, string>) => void;
  /**
   * Checks a trimmed pair before it is added. Returns null to accept, or the message to show; an empty message
   * rejects without showing anything.
   */
  validate?: (key: string, value: string) => string | null;
  /** Selector of the per-row remove buttons, for focus after removal. */
  removeButtonSelector: string;
  /** Default 6000. */
  undoWindowMs?: number;
}

export interface KeyValueList {
  entries: [string, string][];
  keyDraft: string;
  setKeyDraft: (next: string) => void;
  valueDraft: string;
  setValueDraft: (next: string) => void;
  error: string;
  add: () => void;
  pendingRemoval: string | null;
  requestRemove: (key: string) => void;
  cancelRemove: () => void;
  confirmRemove: () => void;
  undo: { key: string; value: string } | null;
  undoRemove: () => void;
  confirmDialogRef: RefObject<HTMLDialogElement | null>;
  keyInputRef: RefObject<HTMLInputElement | null>;
}

export function useKeyValueList(options: UseKeyValueListOptions): KeyValueList {
  const { value, save, validate, removeButtonSelector, undoWindowMs } = options;
  const [keyDraft, setKeyDraft] = useState("");
  const [valueDraft, setValueDraft] = useState("");
  const [error, setError] = useState("");
  const [focusAfterRemoval, setFocusAfterRemoval] = useState<{ key: string; index: number } | null>(null);
  const removal = useRemovalFlow<string, { key: string; value: string }>({ undoWindowMs });
  const keyInputRef = useRef<HTMLInputElement>(null);
  const entries = Object.entries(value ?? {});

  useEffect(() => {
    if (!focusAfterRemoval || entries.some(([key]) => key === focusAfterRemoval.key)) return;
    focusRemoveButton(removeButtonSelector, focusAfterRemoval.index, keyInputRef);
    setFocusAfterRemoval(null);
  }, [entries, focusAfterRemoval, removeButtonSelector]);

  const add = useCallback(() => {
    const key = keyDraft.trim();
    const next = valueDraft.trim();
    setError("");
    const rejected = validate?.(key, next);
    if (rejected != null) {
      setError(rejected);
      return;
    }
    save({ ...(value ?? {}), [key]: next });
    setKeyDraft("");
    setValueDraft("");
  }, [keyDraft, valueDraft, validate, save, value]);

  const confirmRemove = useCallback(() => {
    const key = removal.take();
    if (key === null) return;
    const removed = value?.[key];
    const next = { ...(value ?? {}) };
    delete next[key];
    setFocusAfterRemoval({
      key,
      index: Math.max(
        0,
        entries.findIndex(([entryKey]) => entryKey === key),
      ),
    });
    save(next);
    if (removed === undefined) return;
    removal.offerUndo({ key, value: removed });
  }, [removal, value, entries, save]);

  const undoRemove = useCallback(() => {
    const snapshot = removal.takeUndo();
    if (!snapshot) return;
    save({ ...(value ?? {}), [snapshot.key]: snapshot.value });
  }, [removal, value, save]);

  return {
    entries,
    keyDraft,
    setKeyDraft,
    valueDraft,
    setValueDraft,
    error,
    add,
    pendingRemoval: removal.pending,
    requestRemove: removal.request,
    cancelRemove: removal.cancel,
    confirmRemove,
    undo: removal.undo,
    undoRemove,
    confirmDialogRef: removal.dialogRef,
    keyInputRef,
  };
}
