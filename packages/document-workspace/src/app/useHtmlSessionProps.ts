import { useCallback, useRef, type ComponentProps, type MutableRefObject } from "react";
import type { DocumentRouter } from "../documents/DocumentRouter";
import type { HtmlSessionPages } from "@rightkit/html-editor/documents/html/sessionTypes";
import type { DocumentTabs } from "./useDocumentTabs";
import type { useHtmlSession } from "./useHtmlSession";
import type { useDocumentWatcher } from "./useDocumentWatcher";

// Shared stable "no sub-pages yet" reference for htmlPropsFor's per-path cache below — a fresh `{}`
// literal on every miss would defeat the cache's identity check for a session with no sub-pages.
const EMPTY_HTML_SESSION_PAGE: HtmlSessionPages[string] = {};

type HtmlSession = ReturnType<typeof useHtmlSession>;

interface HtmlSessionPropsDeps {
  htmlPages: DocumentTabs["htmlPages"];
  /** App-owned durable scroll store (survives tab-switch unmounts); read/written per entry path. */
  previewScrollStore: MutableRefObject<Map<string, number>>;
  ensureHtmlPage: HtmlSession["ensureHtmlPage"];
  setHtmlPageEdited: HtmlSession["setHtmlPageEdited"];
  setHtmlActive: HtmlSession["setHtmlActive"];
  saveHtmlPage: HtmlSession["saveHtmlPage"];
  discardHtmlPage: HtmlSession["discardHtmlPage"];
  htmlDirtyPages: HtmlSession["htmlDirtyPages"];
  reload: ReturnType<typeof useDocumentWatcher>["reload"];
}

/**
 * HTML site-session props for the router. Sub-page buffers come from tabs; renderedRevision
 * (checksum) is sole owner of preview reload. Per-html-tab session props (keyed by entry path), so
 * each warm pane is wired to its OWN session. Cache a props object per entryPath and only replace it
 * when THAT path's own pageBuffers/dirty slice actually changed (writeHtmlPages only replaces touched
 * entryPath's sub-object, so unrelated tabs keep old reference — cache hits and same object returned).
 * Extracted verbatim from App.tsx; bodies are unchanged.
 * `dropHtmlProps` is the stable callback App's shared closed-tab eviction effect uses to drop a
 * closed path's cache entry alongside its other per-path caches.
 */
export function useHtmlSessionProps({
  htmlPages,
  previewScrollStore,
  ensureHtmlPage,
  setHtmlPageEdited,
  setHtmlActive,
  saveHtmlPage,
  discardHtmlPage,
  htmlDirtyPages,
  reload,
}: HtmlSessionPropsDeps) {
  const htmlPropsCacheRef = useRef<
    Map<
      string,
      {
        pageBuffers: HtmlSessionPages[string];
        dirtyKey: string;
        props: NonNullable<ComponentProps<typeof DocumentRouter>["html"]>;
      }
    >
  >(new Map());
  const htmlPropsFor = useCallback(
    (entryPath: string): NonNullable<ComponentProps<typeof DocumentRouter>["html"]> => {
      const pageBuffers: HtmlSessionPages[string] = htmlPages[entryPath] ?? EMPTY_HTML_SESSION_PAGE;
      const dirty = htmlDirtyPages(entryPath).map((d) => d.path);
      const dirtyKey = dirty.join(" ");
      const cache = htmlPropsCacheRef.current;
      const cached = cache.get(entryPath);
      if (cached && cached.pageBuffers === pageBuffers && cached.dirtyKey === dirtyKey) {
        return cached.props;
      }
      const props = {
        pageBuffers,
        dirtyPagePaths: dirty,
        onEnsurePage: ensureHtmlPage,
        onPageChange: setHtmlPageEdited,
        onSetActivePage: setHtmlActive,
        onSavePage: saveHtmlPage,
        onDiscardPage: discardHtmlPage,
        onReloadFromDisk: reload,
        initialPreviewScrollY: previewScrollStore.current.get(entryPath) ?? 0,
        onPreviewScroll: (y: number) => previewScrollStore.current.set(entryPath, y),
      };
      cache.set(entryPath, { pageBuffers, dirtyKey, props });
      return props;
    },
    [
      discardHtmlPage,
      ensureHtmlPage,
      htmlDirtyPages,
      reload,
      saveHtmlPage,
      setHtmlActive,
      setHtmlPageEdited,
      htmlPages,
      previewScrollStore,
    ],
  );
  // Closed-tab eviction: drop a closed path's cached props entry alongside its other per-path
  // caches. Stable identity so App's shared eviction effect can list it in deps.
  const dropHtmlProps = useCallback((entryPath: string) => {
    htmlPropsCacheRef.current.delete(entryPath);
  }, []);
  return { htmlPropsFor, dropHtmlProps };
}
