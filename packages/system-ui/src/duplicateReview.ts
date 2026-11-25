import { useEffect, useState, useSyncExternalStore } from "react";
import type { DuplicateReport, DuplicatesTrashResult, SystemClient } from "./api.js";

type ReviewPort = Pick<SystemClient, "homePath" | "duplicatesScan" | "duplicatesTrash">;
interface Item { kept: string; path: string; bytes: number }
interface ReviewState {
  folder: string;
  draft: string;
  report: DuplicateReport | null;
  busy: boolean;
  error: string | null;
  result: DuplicatesTrashResult | null;
}

/** Create once per host review. Operations & results survive view unmounts. */
export function createDuplicateReview(port: ReviewPort) {
  let state: ReviewState = { folder: "", draft: "", report: null, busy: false, error: null, result: null };
  const listeners = new Set<() => void>();
  let draftRevision = 0;

  function update(changes: Partial<ReviewState>) {
    state = { ...state, ...changes };
    listeners.forEach((notify) => notify());
  }

  async function chooseHome() {
    const requested = ++draftRevision;
    try {
      const home = await port.homePath();
      if (requested === draftRevision) update({ draft: home });
    } catch {
      // Keep the editable path when home is unavailable.
    }
  }

  async function find() {
    const folder = state.draft.trim();
    if (!folder || state.busy) return;
    draftRevision++;
    update({ busy: true, error: null, result: null });
    try {
      const report = await port.duplicatesScan(folder);
      update({ report, folder, busy: false });
    } catch (cause) {
      update({ report: null, busy: false, error: String(cause) });
    }
  }

  async function moveToTrash(reviewed: DuplicateReport, items: Item[]) {
    if (state.busy || state.report !== reviewed || !items.length) return;
    draftRevision++;
    update({ busy: true, error: null, result: null });
    try {
      const result = await port.duplicatesTrash(items.map(({ kept, path }) => ({ kept, path })));
      const moved = new Set(result.moved.map(({ path }) => path));
      const groups = reviewed.groups.map((group) => ({
        ...group, extras: group.extras.filter((path) => !moved.has(path)),
      })).filter((group) => group.extras.length > 0);
      update({ busy: false, result, report: { ...reviewed, groups } });
    } catch (cause) {
      // A transport failure can arrive after the host moved files. Require a fresh scan.
      update({ report: null, busy: false, error: String(cause) });
    }
  }

  return {
    snapshot: () => state,
    subscribe(notify: () => void) {
      listeners.add(notify);
      return () => { listeners.delete(notify); };
    },
    initialize() { if (draftRevision === 0) void chooseHome(); },
    setDraft(draft: string) { draftRevision++; update({ draft }); },
    chooseHome, find, moveToTrash,
  };
}

type Review = ReturnType<typeof createDuplicateReview>;
type Selection = { title: string; items: Item[] };

export function useDuplicateReview(review: Review) {
  const state = useSyncExternalStore(review.subscribe, review.snapshot, review.snapshot);
  const [selection, select] = useState<{ review: Review; report: DuplicateReport; value: Selection } | null>(null);
  useEffect(() => { review.initialize(); }, [review]);
  const groups = state.report?.groups ?? [];
  const pending = selection?.review === review && selection.report === state.report && !state.busy ? selection.value : null;
  function setPending(value: Selection | null) {
    select(value && state.report ? { review, report: state.report, value } : null);
  }
  function moveToTrash(items: Item[]) {
    select(null);
    if (selection && selection.review === review) void review.moveToTrash(selection.report, items);
  }
  return {
    ...state, groups, pending, setPending, moveToTrash,
    reclaimable: groups.reduce((sum, group) => sum + group.extras.length * group.size_bytes, 0),
    itemsFor: (kept: string, bytes: number, paths: string[]) => paths.map((path) => ({ kept, bytes, path })),
    chooseHome: review.chooseHome, setDraft: review.setDraft, find: review.find,
  };
}
