/**
 * Markdown core provider.
 *
 * Creates a {@link MarkdownDocumentCore} for the current document,
 * keeps it in sync with the editor's source text, and exposes the
 * core via a React context. Children consume the core via
 * {@link useMarkdownCoreContext}.
 *
 * The provider publishes core commits through the existing
 * `onChange` persistence path. Editor surfaces read & write through
 * this core; analysis surfaces consume the same context.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { MarkdownDocumentCore } from "./core/MarkdownDocumentCore.js";

interface MarkdownCoreContextValue {
  readonly core: MarkdownDocumentCore;
}

const MarkdownCoreContext = createContext<MarkdownCoreContextValue | null>(null);

export interface MarkdownCoreProviderProps {
  readonly documentId: string;
  readonly initialText: string;
  readonly onChange: (rawSource: string) => void;
  /** Edit-store subscription, snapshot, and external-write sequence (origin tag). */
  readonly subscribeEdits?: (path: string, cb: () => void) => () => void;
  readonly getEditSnapshot?: (path: string) => string | undefined;
  readonly getExternalSeq?: (path: string) => number;
  readonly children: ReactNode;
}

/**
 * Wrap children in one Markdown core for this mounted tab. Path changes
 * do not replace its history; unmount disposes it.
 */
export function MarkdownCoreProvider({
  documentId,
  initialText,
  onChange,
  subscribeEdits,
  getEditSnapshot,
  getExternalSeq,
  children,
}: MarkdownCoreProviderProps): ReactNode {
  const publisherRef = useRef(onChange);
  // Recent texts this core published. A save stores one of them as doc.content, so a later
  // initialText equal to any of them is our own echo, never a disk change.
  const publishedRef = useRef<string[]>([]);
  const [publish] = useState(() => (rawSource: string) => {
    const recent = publishedRef.current;
    recent.push(rawSource);
    if (recent.length > 32) recent.shift();
    publisherRef.current(rawSource);
  });
  const [core] = useState(() =>
    MarkdownDocumentCore.create({
      documentId,
      initialLogicalSource: initialText,
      publishRawSource: publish,
    }),
  );
  useLayoutEffect(() => {
    publisherRef.current = onChange;
    core.setPublisher(publish);
  }, [core, onChange, publish]);

  // Sole external-ingress owner. Core publishes go through writeEditsLive and never advance the
  // external sequence; only writeEdits (reopen/reload/restore/conflict/duplicate) does. A string
  // comparison cannot tell a stale echo from a real external write, so origin is tagged instead:
  // apply external text exactly once per sequence advance, ignore everything else.
  const apiRef = useRef({ subscribeEdits, getEditSnapshot, getExternalSeq });
  apiRef.current = { subscribeEdits, getEditSnapshot, getExternalSeq };
  const appliedSeqRef = useRef<number | null>(null);
  // Set when an external write deleted the edit entry (reload/close); only then may doc.content apply.
  const pendingBaseRef = useRef(false);
  if (appliedSeqRef.current === null) appliedSeqRef.current = getExternalSeq?.(documentId) ?? 0;
  const applyExternal = useCallback(
    (next: string, source: "provider", seq: number) => {
      const revision = core.snapshot().revision;
      core.replaceFromExternal(next);
      const after = core.snapshot().revision;
      if (after !== revision)
        console.info(
          JSON.stringify({
            event: "md_external_replace_applied",
            source,
            seq,
            path: documentId,
            nextLength: next.length,
            revision: after,
          }),
        );
    },
    [core, documentId],
  );
  useEffect(() => {
    const check = () => {
      const { getExternalSeq: seqOf, getEditSnapshot: snap } = apiRef.current;
      if (!seqOf) return;
      const seq = seqOf(documentId);
      if (seq <= (appliedSeqRef.current ?? 0)) return;
      appliedSeqRef.current = seq;
      const next = snap?.(documentId);
      // Deleted edit entry (reload/close): the new text is doc.content (initialText), which may not
      // have rendered yet; the initialText effect below applies it.
      if (next !== undefined) applyExternal(next, "provider", seq);
      else pendingBaseRef.current = true;
    };
    check();
    const subscribe = apiRef.current.subscribeEdits;
    return subscribe ? subscribe(documentId, check) : undefined;
  }, [documentId, applyExternal]);

  // Reload case: no edit entry, disk content changed. replaceFromExternal is itself a no-op when
  // the text equals the core's published raw source.
  // doc.content (initialText) only changes when the file is (re)read from disk; re-runs with an
  // unchanged initialText are render echoes and must not revert newer typing in the core.
  const lastInitialRef = useRef(initialText);
  useEffect(() => {
    const changed =
      initialText !== lastInitialRef.current && !publishedRef.current.includes(initialText);
    lastInitialRef.current = initialText;
    if (!changed && !pendingBaseRef.current) return;
    if (apiRef.current.getEditSnapshot?.(documentId) !== undefined) return;
    pendingBaseRef.current = false;
    applyExternal(initialText, "provider", appliedSeqRef.current ?? 0);
  }, [initialText, documentId, applyExternal]);

  useEffect(() => {
    if (
      import.meta.env?.VITE_RIGHTKIT_BROWSER_QA !== "1" ||
      new URLSearchParams(window.location.search).get("qa") !== "1"
    )
      return;
    let cancelled = false;
    let stop: (() => void) | undefined;
    void import("./qaCoreObservation.js").then(({ observeQaMarkdownCore }) => {
      if (!cancelled) stop = observeQaMarkdownCore(documentId, core);
    });
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [core, documentId]);

  // Mount-generation ownership: React 19 StrictMode mounts, unmounts,
  // and immediately remounts this provider synchronously (in the same
  // tick, with no render in between) to flush out lifecycle bugs. A
  // naive `dispose()` in the cleanup would dispose `core` before the
  // synthetic remount's effects re-run against the very same instance,
  // so every other effect that touches `core` (this one included, plus
  // {@link useMarkdownCoreSubscription} elsewhere in the tree) would
  // throw "cannot ... in lifecycle 'disposed'".
  //
  // Each effect run claims the current generation and defers the
  // actual dispose to a microtask. If the effect re-runs synchronously
  // before that microtask fires (the StrictMode case), the generation
  // has already moved on and the deferred dispose is a no-op — the
  // live generation keeps its still-`ready` core. On a genuine unmount
  // nothing re-runs to bump the generation, so the microtask observes
  // a match and disposes for real.
  const generationRef = useRef(0);
  useEffect(() => {
    const myGeneration = ++generationRef.current;
    return () => {
      queueMicrotask(() => {
        if (generationRef.current === myGeneration) {
          core.dispose();
        }
      });
    };
  }, [core]);

  return <MarkdownCoreContext.Provider value={{ core }}>{children}</MarkdownCoreContext.Provider>;
}

/**
 * Read the core from the nearest provider. Throws when no provider
 * is mounted.
 */
export function useMarkdownCoreContext(): MarkdownDocumentCore {
  const value = useContext(MarkdownCoreContext);
  if (!value) {
    throw new Error("useMarkdownCoreContext must be used inside <MarkdownCoreProvider>");
  }
  return value.core;
}

/**
 * Read the core from the nearest provider, or `null` if no provider
 * is mounted. Useful for components that can render both inside and
 * outside a core context.
 */
export function useMarkdownCoreOptional(): MarkdownDocumentCore | null {
  const value = useContext(MarkdownCoreContext);
  return value?.core ?? null;
}
