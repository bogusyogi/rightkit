import { useCallback, useRef, useState } from "react";
import type { HtmlPageBuffer } from "./sessionTypes";
import { commitHtmlMutation, type HtmlMutationPlan } from "./htmlMutationEngine";

interface Args {
  entryPath: string;
  activePath: string;
  text: string;
  pageBuffers: Record<string, HtmlPageBuffer>;
  dirtyPagePaths: string[];
  onChange: (text: string) => void;
  onPageChange: (entryPath: string, pagePath: string, text: string) => void;
}

/**
 * Owner of "which bytes are current for the active page": the entry's shared text (with a locally
 * parked commit until the frozen `text` prop catches up) or a sub-page buffer, plus the revision
 * token and the single mutation-commit entry point used by every visual edit.
 */
export function useHtmlPageBuffer({
  entryPath,
  activePath,
  text,
  pageBuffers,
  dirtyPagePaths,
  onChange,
  onPageChange,
}: Args) {
  const onEntry = activePath === entryPath;
  // A rendered-pane (double-click) commit writes through `onChange` into the app's edit store, which
  // deliberately mutates a ref WITHOUT re-rendering App — keystrokes must not repaint heavy viewers.
  // The upshot is that the `text` prop this pane receives stays frozen at the pre-edit bytes, so the
  // snapshot effect would read those stale bytes, find a checksum that disagrees with the revision
  // it just rendered, and overwrite the edit with the original. Hold our own committed source as
  // the entry buffer until the prop catches up. Sub-pages never needed this: `onPageChange`
  // updates app STATE, so `pageBuffers` re-renders with the edit already in it.
  const [committedSource, setCommittedSource] = useState<string | null>(null);
  // Any real change to `text` — disk reload, the watcher, a discard, or simply the store catching up
  // — is authority and supersedes the local commit. Adjusted during render rather than in an effect
  // so the override is gone in the SAME pass that delivers the new prop.
  const [seenText, setSeenText] = useState(text);
  if (text !== seenText) {
    setSeenText(text);
    if (committedSource !== null) setCommittedSource(null);
  }
  const activeBuffer = onEntry
    ? (committedSource ?? text)
    : (pageBuffers[activePath]?.edited ?? "");
  const commitSource = useCallback(
    (nextSource: string) => {
      if (onEntry) {
        setCommittedSource(nextSource);
        onChange(nextSource);
      } else onPageChange(entryPath, activePath, nextSource);
    },
    [activePath, entryPath, onChange, onEntry, onPageChange],
  );
  const activeBufferRef = useRef(activeBuffer);
  activeBufferRef.current = activeBuffer;
  // Both page kinds own a counter-issued revision token. Sub-pages take it from
  // `pageBuffers[...].revision`. The entry page keeps a bounded ledger of bytes -> token: a byte
  // state never seen before is issued the next counter value (strictly increasing, no checksum
  // collisions), and a byte state seen before keeps its token. The second half is what undo needs:
  // history entries stamp `expectedRevision` per byte state, so reverting to bytes B1 must give B1
  // back the SAME token its older inverse plans were stamped with. A pure "advance on every byte
  // change" counter re-issues a new token on revert and rejects every earlier inverse as stale.
  // `revisionForSource` is a pure peek (unknown bytes -> the value they WOULD be issued);
  // `adoptEntrySource` is the only writer and is idempotent by bytes, so a commit adopting its
  // candidate and the later render delivering those same bytes agree. Evicted bytes get a fresh
  // token, which fails closed (stale) instead of guessing.
  const pageRevision = onEntry ? undefined : (pageBuffers[activePath]?.revision ?? 0);
  const pageRevisionRef = useRef({ path: activePath, value: pageRevision ?? 0 });
  if (pageRevisionRef.current.path !== activePath) {
    pageRevisionRef.current = { path: activePath, value: pageRevision ?? 0 };
  } else if (pageRevision !== undefined && pageRevision > pageRevisionRef.current.value) {
    pageRevisionRef.current.value = pageRevision;
  }
  const entryLedgerRef = useRef<{ tokens: Map<string, number>; next: number }>({
    tokens: new Map(),
    next: 0,
  });
  const adoptEntrySource = useCallback((source: string) => {
    const ledger = entryLedgerRef.current;
    if (ledger.tokens.has(source)) return;
    ledger.tokens.set(source, ledger.next);
    ledger.next += 1;
    if (ledger.tokens.size > 256) {
      const oldest = ledger.tokens.keys().next().value;
      if (oldest !== undefined) ledger.tokens.delete(oldest);
    }
  }, []);
  if (onEntry) adoptEntrySource(activeBuffer);
  const revisionForSource = useCallback(
    (source: string) => {
      if (onEntry) {
        const ledger = entryLedgerRef.current;
        return ledger.tokens.get(source) ?? ledger.next;
      }
      return source === activeBufferRef.current
        ? pageRevisionRef.current.value
        : pageRevisionRef.current.value + 1;
    },
    [onEntry],
  );
  const commitPageMutation = useCallback(
    (source: string, plan: HtmlMutationPlan) => {
      const committed = commitHtmlMutation(source, plan, { revisionOf: revisionForSource });
      if (onEntry) adoptEntrySource(committed.source);
      else pageRevisionRef.current.value += 1;
      return committed;
    },
    [onEntry, revisionForSource, adoptEntrySource],
  );
  // Entry edits update App's store without re-rendering App, so `dirtyPagePaths` lags them until the
  // next navigation; the locally parked commit is the immediate signal for the entry page.
  const activeDirty = onEntry
    ? committedSource !== null
      ? committedSource !== text
      : dirtyPagePaths.includes(activePath)
    : dirtyPagePaths.includes(activePath);
  const subLoaded = onEntry || !!pageBuffers[activePath];

  return {
    onEntry,
    activeBuffer,
    activeBufferRef,
    activeDirty,
    subLoaded,
    setCommittedSource,
    commitSource,
    revisionForSource,
    commitPageMutation,
  };
}
