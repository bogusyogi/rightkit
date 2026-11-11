// Vocabulary page - CRUD for customer recognition bias.
// NOTE: engine-side decode biasing IS active - saved vocabulary terms install as
// Parakeet context-bias phrases in the heardright-engine sidecar.
// Command bias (zephyr stop/send/…) is separate + internal/dev-only, not a customer setting.
//
// Layout: Vocabulary, Snippets and Text replacements are three always-visible
// sections, side by side on a wide page and stacked on a narrow one. Each is a
// header (title, count, optional action) over add row -> note -> hairline rows.

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { Plus, UploadSimple, X } from "@phosphor-icons/react";
import { dictationClient } from "../host.js";
import type { VocabularyEntry, VocabularySuggestion } from "../types/vocabulary.js";
import ErrorBanner from "../components/ErrorBanner.js";
import { cx, ui } from "../lib/ui.js";
import { useSettings } from "../hooks/useSettings.js";
import {
  DictationPanel,
  dictationAddButton,
  dictationEmpty,
  dictationFormColumns,
  dictationInput,
  dictationRow,
  dictationRowRemove,
} from "../components/settings/DictationSection.js";
import SnippetsSettings from "../components/settings/SnippetsSettings.js";
import ReplacementsSettings from "../components/settings/ReplacementsSettings.js";

const AddIcon = memo(function AddIcon() {
  return <Plus aria-hidden="true" className="h-4 w-4" />;
});

const UploadIcon = memo(function UploadIcon() {
  return <UploadSimple aria-hidden="true" className="h-4 w-4" />;
});

const ChipRemoveIcon = memo(function ChipRemoveIcon() {
  return <X aria-hidden="true" className="h-3.5 w-3.5" weight="bold" />;
});

const UNDO_WINDOW_MS = 6000;

export default function VocabularyPage() {
  const [entries, setEntries] = useState<VocabularyEntry[]>([]);
  const [suggestions, setSuggestions] = useState<VocabularySuggestion[]>([]);
  const [newTerm, setNewTerm] = useState("");
  const [newAliases, setNewAliases] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [pendingRemoval, setPendingRemoval] = useState<VocabularyEntry | null>(null);
  const [focusAfterRemovalIndex, setFocusAfterRemovalIndex] = useState<number | null>(null);
  // Snapshot for the undo toast. `remove_vocabulary` already committed by the
  // time this shows; undo re-adds via `add_vocabulary` (a new row/id — the
  // goal is restoring the term to your vocabulary, not the exact old row).
  const [undo, setUndo] = useState<VocabularyEntry | null>(null);
  const undoTimerRef = useRef<number | undefined>(undefined);
  const confirmDialogRef = useRef<HTMLDialogElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const newTermInputRef = useRef<HTMLInputElement>(null);
  const settings = useSettings();

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
    if (focusAfterRemovalIndex === null) return;
    const removeButtons = document.querySelectorAll<HTMLButtonElement>("[data-vocabulary-remove]");
    const nextButton =
      removeButtons.length > 0
        ? removeButtons.item(Math.min(focusAfterRemovalIndex, removeButtons.length - 1))
        : null;
    if (nextButton) nextButton.focus();
    else newTermInputRef.current?.focus();
    setFocusAfterRemovalIndex(null);
  }, [focusAfterRemovalIndex]);

  const refresh = useCallback(async () => {
    try {
      const [list, sug] = await Promise.all([
        dictationClient().listVocabulary(),
        dictationClient().listVocabularySuggestions(),
      ]);
      setEntries(list.sort((a, b) => b.created_at_ms - a.created_at_ms));
      // Multi-token phrase corrections surface immediately; single-token
      // suggestions require `seen >= 2` so one-off typo edits don't shout.
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

  async function add() {
    const term = newTerm.trim();
    if (term.length === 0 || busy) return;
    setBusy(true);
    setError("");
    try {
      const aliases = newAliases
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
      const created = await dictationClient().addVocabulary(term, aliases.length > 0 ? aliases : undefined);
      setEntries((prev) => [created, ...prev.filter((e) => e.id !== created.id)]);
      setSuggestions((prev) => prev.filter((s) => s.term.toLowerCase() !== term.toLowerCase()));
      setNewTerm("");
      setNewAliases("");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onImportFile(event: React.ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (!file) return;
    setBusy(true);
    setError("");
    try {
      const text = await file.text();
      const terms = text
        .split(/\r?\n/)
        .map((s) => s.trim())
        .filter((s) => s.length > 0 && s.length <= 200);
      await dictationClient().importVocabulary(terms);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  }

  const requestRemove = useCallback((entry: VocabularyEntry) => {
    setPendingRemoval(entry);
  }, []);

  const confirmRemove = useCallback(async () => {
    const entry = pendingRemoval;
    setPendingRemoval(null);
    if (!entry) return;
    setError("");
    try {
      const removalIndex = entries.findIndex((current) => current.id === entry.id);
      const removed = await dictationClient().removeVocabulary(entry.id);
      if (!removed) throw new Error("Vocabulary entry was not found.");
      await refresh();
      setFocusAfterRemovalIndex(Math.max(0, removalIndex));
      if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
      setUndo(entry);
      undoTimerRef.current = window.setTimeout(() => {
        undoTimerRef.current = undefined;
        setUndo(null);
      }, UNDO_WINDOW_MS);
    } catch (e) {
      setError(String(e));
    }
  }, [entries, pendingRemoval, refresh]);

  const undoRemoveTerm = useCallback(async () => {
    const entry = undo;
    if (!entry) return;
    if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
    setUndo(null);
    setError("");
    try {
      await dictationClient().addVocabulary(
        entry.term,
        entry.sounds_like && entry.sounds_like.length > 0 ? entry.sounds_like : undefined,
      );
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [undo, refresh]);

  const acceptSuggestion = useCallback(
    async (term: string) => {
      setError("");
      try {
        // The store promotes the suggestion (keeping the misheard form as a sound-alike) and removes it; add_vocabulary would do neither.
        await dictationClient().acceptVocabularySuggestion(term);
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
      await dictationClient().dismissVocabularySuggestion(term);
      setSuggestions((prev) => prev.filter((s) => s.term !== term));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const snippets = settings.get("snippets", {} as Record<string, string>);
  const replacements = settings.get("replacements", {} as Record<string, string>);
  const counts = {
    vocabulary: entries.length,
    snippets: Object.keys(snippets).length,
    replacements: Object.keys(replacements).length,
  };

  return (
    <div className="@container flex min-h-0 flex-col">
      {error ? <ErrorBanner message={error} onDismiss={() => setError("")} /> : null}
      {settings.error ? (
        <ErrorBanner message={settings.error} onDismiss={() => settings.setError("")} />
      ) : null}

      <input
        ref={fileInputRef}
        type="file"
        accept=".txt"
        className="hidden"
        onChange={(e) => void onImportFile(e)}
      />

      {/* All three lists stay visible: side by side when the page is wide,
          stacked when it is not. No tabs to hunt through. */}
      <div className="grid min-w-0 items-stretch gap-[var(--space-3)] @[1080px]:grid-cols-3">
        <DictationSectionCard
          title="Vocabulary"
          count={counts.vocabulary}
          settingId="dictation.vocabulary"
          action={
            <button
              type="button"
              className={cx(
                ui.button,
                ui.buttonGhost,
                ui.buttonCompact,
                "!h-7 !min-h-7 gap-[6px] !border-transparent !bg-transparent !px-2 text-[var(--muted)] hover:!bg-[var(--surface-hover)] hover:text-[var(--paper)]",
              )}
              onClick={() => fileInputRef.current?.click()}
              disabled={busy}
            >
              <UploadIcon />
              Import .txt
            </button>
          }
        >
          {suggestions.length > 0 ? (
            <div className="mb-[var(--space-2)] overflow-hidden rounded-[var(--radius-sm)] border-l-[3px] border-[var(--secondary)] bg-[color-mix(in_srgb,var(--secondary)_8%,var(--surface-1))]">
              <div className="px-[var(--space-3)] pb-[2px] pt-[var(--space-2)] text-[length:var(--size-xs)] font-semibold text-[var(--secondary)]">
                Suggested from your edits
              </div>
              {suggestions.map((suggestion) => (
                <div
                  key={suggestion.term}
                  className="flex min-h-[40px] items-center gap-[var(--space-2)] px-[var(--space-3)] py-[6px]"
                >
                  <div className="min-w-0 flex-1">
                    <span className="block truncate font-semibold text-[var(--paper)]">
                      {suggestion.term}
                    </span>
                    <span className="block truncate text-[length:var(--size-xs)] text-[var(--muted)]">
                      heard as &ldquo;{suggestion.heard_as}&rdquo;
                      {suggestion.seen > 1 ? ` · seen ${suggestion.seen}×` : ""}
                    </span>
                  </div>
                  <button
                    type="button"
                    className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact)}
                    onClick={() => void acceptSuggestion(suggestion.term)}
                    aria-label={`Add ${suggestion.term} to vocabulary`}
                  >
                    Add
                  </button>
                  <button
                    type="button"
                    className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact)}
                    onClick={() => void dismissSuggestion(suggestion.term)}
                    aria-label={`Ignore suggestion ${suggestion.term}`}
                  >
                    Ignore
                  </button>
                </div>
              ))}
            </div>
          ) : null}

          <DictationPanel
            form={
              <div className={dictationFormColumns}>
                <input
                  ref={newTermInputRef}
                  type="text"
                  aria-label="Word or name"
                  placeholder="Word or name"
                  value={newTerm}
                  onChange={(e) => setNewTerm(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void add();
                  }}
                  className={dictationInput}
                  disabled={busy}
                  maxLength={200}
                />
                <input
                  type="text"
                  aria-label="Sounds like (optional, comma-separated)"
                  placeholder="Sounds like"
                  value={newAliases}
                  onChange={(e) => setNewAliases(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void add();
                  }}
                  className={dictationInput}
                  disabled={busy}
                  maxLength={1000}
                />
                <button
                  type="button"
                  className={dictationAddButton}
                  onClick={() => void add()}
                  disabled={busy || newTerm.trim().length === 0}
                >
                  <AddIcon />
                  Add
                </button>
              </div>
            }
            note="Up to 200 characters per term, up to 5 sound-alikes. Edit a delivered dictation and we’ll suggest words."
          >
            {!loaded ? (
              <div className={dictationEmpty}>Loading vocabulary…</div>
            ) : entries.length === 0 ? (
              <div className={dictationEmpty}>No words yet. Add one above or import a .txt.</div>
            ) : (
              entries.map((entry) => (
                <div key={entry.id} className={dictationRow}>
                  <span className="min-w-0 truncate font-semibold text-[var(--paper)]">
                    {entry.term}
                  </span>
                  <span className="min-w-0 truncate text-[length:var(--size-xs)] text-[var(--muted)]">
                    {entry.sounds_like && entry.sounds_like.length > 0
                      ? entry.sounds_like.join(", ")
                      : ""}
                  </span>
                  <button
                    type="button"
                    data-vocabulary-remove={entry.id}
                    className={dictationRowRemove}
                    onClick={() => requestRemove(entry)}
                    aria-label={`Remove ${entry.term}`}
                    title="Remove"
                  >
                    <ChipRemoveIcon />
                  </button>
                </div>
              ))
            )}
          </DictationPanel>
        </DictationSectionCard>

        <DictationSectionCard
          title="Snippets"
          count={counts.snippets}
          settingId="dictation.snippets"
        >
          <SnippetsSettings loaded={settings.loaded} snippets={snippets} patch={settings.patch} />
        </DictationSectionCard>

        <DictationSectionCard
          title="Text replacements"
          count={counts.replacements}
          settingId="dictation.text-replacements"
        >
          <ReplacementsSettings
            loaded={settings.loaded}
            replacements={replacements}
            patch={settings.patch}
          />
        </DictationSectionCard>
      </div>

      {pendingRemoval ? (
        <dialog
          ref={confirmDialogRef}
          aria-labelledby="vocab-remove-title"
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
            id="vocab-remove-title"
            className="m-0 [font-family:var(--font-display)] text-[length:var(--size-xl)] text-[var(--paper)]"
          >
            Remove this word?
          </h2>
          <p className="m-0 mt-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--muted)]">
            <span className="[font-family:var(--font-mono)]">{pendingRemoval.term}</span> will no
            longer be prioritised during dictation.
          </p>
          <div className="mt-[var(--space-4)] flex items-center justify-end gap-[var(--space-2)]">
            <button
              type="button"
              className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
              onClick={() => setPendingRemoval(null)}
            >
              Cancel
            </button>
            <button
              type="button"
              className={cx(ui.button, ui.buttonDanger, ui.buttonCompact)}
              onClick={() => void confirmRemove()}
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
          Removed &ldquo;{undo.term}&rdquo;.
          <button
            type="button"
            className="cursor-pointer rounded-[var(--radius-sm)] border-0 bg-transparent p-0 font-semibold text-[var(--accent-cool)] underline underline-offset-2 focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
            onClick={() => void undoRemoveTerm()}
          >
            Undo
          </button>
        </output>
      ) : null}
    </div>
  );
}

// One always-visible list. The heading carries the settings-search id so search
// scrolls straight to it.
function DictationSectionCard({
  title,
  count,
  settingId,
  action,
  children,
}: {
  title: string;
  count: number;
  settingId: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  const headingId = `dictation-${settingId.split(".")[1]}`;
  return (
    <section
      aria-labelledby={headingId}
      className="flex min-w-0 flex-col gap-[var(--space-2)] rounded-[var(--radius-md)] border border-[var(--line)] bg-[var(--surface-2)] p-[var(--space-3)]"
    >
      <div className="flex min-h-7 items-center gap-[var(--space-2)]">
        <h2
          id={headingId}
          tabIndex={-1}
          data-setting-id={settingId}
          className={cx(
            ui.sectionHeader,
            "focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]",
          )}
        >
          {title}
        </h2>
        <span className="inline-flex min-w-6 items-center justify-center rounded-full bg-[var(--surface-3)] px-[6px] py-[2px] text-[length:var(--size-xs)] font-semibold text-[var(--muted)] [font-variant-numeric:tabular-nums]">
          {count}
        </span>
        {action ? <div className="ml-auto">{action}</div> : null}
      </div>
      {children}
    </section>
  );
}
