// A custom-vocabulary list: load terms and suggestions, add a term with optional sound-alikes, import terms
// from a text file, remove with confirmation and undo, accept or dismiss suggestions. The host supplies the
// store as a port; markup and wording are the host's. Imports only `react`.
import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { focusRemoveButton, useRemovalFlow } from "./removal.js";

export interface VocabularyTerm {
  id: number;
  term: string;
  created_at_ms: number;
  /** Sound-alike aliases. */
  sounds_like?: string[];
}

export interface VocabularySuggestionLike {
  term: string;
  /** How often the correction was seen. */
  seen: number;
}

export interface VocabularyPort<E extends VocabularyTerm, S extends VocabularySuggestionLike> {
  list(): Promise<E[]>;
  listSuggestions(): Promise<S[]>;
  add(term: string, soundsLike: string[] | undefined): Promise<E>;
  importTerms(terms: string[]): Promise<unknown>;
  /** Resolves false when no such entry exists. */
  remove(id: number): Promise<boolean>;
  /** Promotes a suggestion (keeping the misheard form as a sound-alike) and removes it. */
  acceptSuggestion(term: string): Promise<unknown>;
  dismissSuggestion(term: string): Promise<unknown>;
}

export interface UseVocabularyOptions {
  /** Selector of the per-row remove buttons, for focus after removal. */
  removeButtonSelector: string;
  /** Error text when a removal finds no such entry. Default "Vocabulary entry was not found." */
  notFoundMessage?: string;
  /** Longest line accepted from an imported file. Default 200. */
  maxImportTermLength?: number;
  /** Default 6000. */
  undoWindowMs?: number;
}

export interface Vocabulary<E extends VocabularyTerm, S extends VocabularySuggestionLike> {
  entries: E[];
  suggestions: S[];
  newTerm: string;
  setNewTerm: (next: string) => void;
  newAliases: string;
  setNewAliases: (next: string) => void;
  error: string;
  setError: (next: string) => void;
  busy: boolean;
  loaded: boolean;
  refresh: () => Promise<void>;
  add: () => Promise<void>;
  /** `onChange` of a file input: reads the first file, one term per line. */
  importFile: (event: { target: { files: FileList | null } }) => Promise<void>;
  pendingRemoval: E | null;
  requestRemove: (entry: E) => void;
  cancelRemove: () => void;
  confirmRemove: () => Promise<void>;
  undo: E | null;
  undoRemoveTerm: () => Promise<void>;
  acceptSuggestion: (term: string) => Promise<void>;
  dismissSuggestion: (term: string) => Promise<void>;
  confirmDialogRef: RefObject<HTMLDialogElement | null>;
  fileInputRef: RefObject<HTMLInputElement | null>;
  newTermInputRef: RefObject<HTMLInputElement | null>;
}

export function useVocabulary<E extends VocabularyTerm, S extends VocabularySuggestionLike>(
  port: VocabularyPort<E, S>,
  options: UseVocabularyOptions,
): Vocabulary<E, S> {
  const {
    removeButtonSelector,
    notFoundMessage = "Vocabulary entry was not found.",
    maxImportTermLength = 200,
    undoWindowMs,
  } = options;
  const portRef = useRef(port);
  portRef.current = port;
  const [entries, setEntries] = useState<E[]>([]);
  const [suggestions, setSuggestions] = useState<S[]>([]);
  const [newTerm, setNewTerm] = useState("");
  const [newAliases, setNewAliases] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [focusAfterRemovalIndex, setFocusAfterRemovalIndex] = useState<number | null>(null);
  // `remove` already committed by the time the undo offer shows; undo re-adds the term (a new row and id: the
  // goal is restoring the term to the vocabulary, not the exact old row).
  const removal = useRemovalFlow<E>({ undoWindowMs });
  const fileInputRef = useRef<HTMLInputElement>(null);
  const newTermInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (focusAfterRemovalIndex === null) return;
    focusRemoveButton(removeButtonSelector, focusAfterRemovalIndex, newTermInputRef);
    setFocusAfterRemovalIndex(null);
  }, [focusAfterRemovalIndex, removeButtonSelector]);

  const refresh = useCallback(async () => {
    try {
      const [list, sug] = await Promise.all([portRef.current.list(), portRef.current.listSuggestions()]);
      setEntries(list.sort((a, b) => b.created_at_ms - a.created_at_ms));
      // Multi-token phrase corrections surface immediately; single-token suggestions require `seen >= 2`
      // so one-off typo edits do not shout.
      setSuggestions(
        sug.filter((entry) => {
          const multi = entry.term.split(/\s+/).length > 1;
          return multi || entry.seen >= 2;
        }),
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const add = useCallback(async () => {
    const term = newTerm.trim();
    if (term.length === 0 || busy) return;
    setBusy(true);
    setError("");
    try {
      const aliases = newAliases
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
      const created = await portRef.current.add(term, aliases.length > 0 ? aliases : undefined);
      setEntries((prev) => [created, ...prev.filter((e) => e.id !== created.id)]);
      setSuggestions((prev) => prev.filter((s) => s.term.toLowerCase() !== term.toLowerCase()));
      setNewTerm("");
      setNewAliases("");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [newTerm, newAliases, busy]);

  const importFile = useCallback(
    async (event: { target: { files: FileList | null } }) => {
      const file = event.target.files?.[0];
      if (!file) return;
      setBusy(true);
      setError("");
      try {
        const text = await file.text();
        const terms = text
          .split(/\r?\n/)
          .map((s) => s.trim())
          .filter((s) => s.length > 0 && s.length <= maxImportTermLength);
        await portRef.current.importTerms(terms);
        await refresh();
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
        if (fileInputRef.current) fileInputRef.current.value = "";
      }
    },
    [refresh, maxImportTermLength],
  );

  const confirmRemove = useCallback(async () => {
    const entry = removal.take();
    if (!entry) return;
    setError("");
    try {
      const removalIndex = entries.findIndex((current) => current.id === entry.id);
      const removed = await portRef.current.remove(entry.id);
      if (!removed) throw new Error(notFoundMessage);
      await refresh();
      setFocusAfterRemovalIndex(Math.max(0, removalIndex));
      removal.offerUndo(entry);
    } catch (e) {
      setError(String(e));
    }
  }, [removal, entries, refresh, notFoundMessage]);

  const undoRemoveTerm = useCallback(async () => {
    const entry = removal.takeUndo();
    if (!entry) return;
    setError("");
    try {
      await portRef.current.add(
        entry.term,
        entry.sounds_like && entry.sounds_like.length > 0 ? entry.sounds_like : undefined,
      );
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [removal, refresh]);

  const acceptSuggestion = useCallback(
    async (term: string) => {
      setError("");
      try {
        await portRef.current.acceptSuggestion(term);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh],
  );

  const dismissSuggestion = useCallback(async (term: string) => {
    setError("");
    try {
      await portRef.current.dismissSuggestion(term);
      setSuggestions((prev) => prev.filter((s) => s.term !== term));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  return {
    entries,
    suggestions,
    newTerm,
    setNewTerm,
    newAliases,
    setNewAliases,
    error,
    setError,
    busy,
    loaded,
    refresh,
    add,
    importFile,
    pendingRemoval: removal.pending,
    requestRemove: removal.request,
    cancelRemove: removal.cancel,
    confirmRemove,
    undo: removal.undo,
    undoRemoveTerm,
    acceptSuggestion,
    dismissSuggestion,
    confirmDialogRef: removal.dialogRef,
    fileInputRef,
    newTermInputRef,
  };
}
