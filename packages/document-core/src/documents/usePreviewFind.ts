import { useCallback, useEffect, useRef, type RefObject } from "react";
import type { SearchOptions, SearchProvider, SearchResult } from "./types.js";

/** The in-frame find protocol (one set of postMessage senders per preview kind). */
export interface PreviewFindProtocol {
  clear: (iframe: HTMLIFrameElement | null, sessionId: string) => void;
  query: (
    iframe: HTMLIFrameElement | null,
    sessionId: string,
    requestId: number,
    query: string,
    options: SearchOptions,
  ) => void;
  step: (
    iframe: HTMLIFrameElement | null,
    sessionId: string,
    requestId: number,
    delta: number,
  ) => void;
}

interface PreviewFindDeps {
  iframeRef: RefObject<HTMLIFrameElement | null>;
  sessionId: string;
  /** Register the provider only while the frame has findable content (the caller's activation condition). */
  enabled: boolean;
  kind: SearchProvider["kind"];
  protocol: PreviewFindProtocol;
  registerSearchProvider: (provider: SearchProvider | null) => void;
}

/**
 * A SearchProvider that drives an in-frame find engine (interactive HTML or MDX preview) over
 * postMessage. The sandboxed (opaque-origin) frame can't be read by the app, so each find/step posts a
 * requestId and the matching FIND_RESULT — routed in by the caller via the returned `resolveFindResult`
 * — settles the FindBar's promise; stragglers time out at 1500 ms. `useHtmlPreviewFind` and
 * `useMdxPreviewFind` are thin wrappers that supply the per-kind protocol + activation condition.
 */
export function usePreviewFind({
  iframeRef,
  sessionId,
  enabled,
  kind,
  protocol,
  registerSearchProvider,
}: PreviewFindDeps) {
  const findReqRef = useRef(0);
  const findPendingRef = useRef<Map<number, (r: SearchResult) => void>>(new Map());

  const resolveFindResult = useCallback((requestId: number, result: SearchResult) => {
    const resolve = findPendingRef.current.get(requestId);
    if (resolve) {
      findPendingRef.current.delete(requestId);
      resolve(result);
    }
  }, []);

  useEffect(() => {
    if (!enabled) return;
    const post = (send: (id: number) => void): Promise<SearchResult> =>
      new Promise((resolve) => {
        const id = ++findReqRef.current;
        const timeout = window.setTimeout(() => {
          findPendingRef.current.delete(id);
          resolve({ count: 0, activeIndex: -1 });
        }, 1500);
        findPendingRef.current.set(id, (r) => {
          window.clearTimeout(timeout);
          resolve(r);
        });
        send(id);
      });
    const provider: SearchProvider = {
      kind,
      canReplace: false,
      supportsMatchCase: true,
      supportsWholeWord: true,
      supportsRegex: true,
      find: (q, options) => {
        if (!q) {
          protocol.clear(iframeRef.current, sessionId);
          return { count: 0, activeIndex: -1 };
        }
        return post((id) => protocol.query(iframeRef.current, sessionId, id, q, options));
      },
      next: () => post((id) => protocol.step(iframeRef.current, sessionId, id, 1)),
      prev: () => post((id) => protocol.step(iframeRef.current, sessionId, id, -1)),
      close: () => protocol.clear(iframeRef.current, sessionId),
    };
    registerSearchProvider(provider);
    return () => {
      protocol.clear(iframeRef.current, sessionId);
      registerSearchProvider(null);
    };
    // protocol + iframeRef are stable (module-level senders / a ref); re-arm only on the activation inputs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    enabled,
    sessionId,
    kind,
    registerSearchProvider,
    protocol.step,
    protocol.query,
    protocol.clear,
    iframeRef.current,
  ]);

  return { resolveFindResult };
}
