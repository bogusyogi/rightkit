// MdxSplitPane: the MDX branch of the markdown/mdx split pane. Mirrors MarkdownSplitPane
// structurally — same source drawer (CodeMirror markdown mode + markdownToolbar), same
// find-provider-publish pattern, same source-fullscreen / focus / typewriter, same outline.
//
// The preview slot is the only difference: instead of mounting <MarkdownView> in the app's
// React tree, it mounts <MdxPreviewFrame> — a sandboxed iframe (sandbox="allow-scripts", never
// allow-same-origin) where the compiled MDX renders. The duplication vs MarkdownSplitPane is
// intentional: each format owns its own split pane so changes to one don't accidentally affect
// the other. The shared code (TextEditor, MarkdownSourceToolbar, MarkdownSourceActions, etc.)
// lives in the markdown/ folder; this file reuses them by import.

import { useCloseFindOnSurfaceChange } from "@rightkit/document-core/app/useCloseFindOnSurfaceChange";
import { useCallback, useEffect, useRef, type ReactNode } from "react";
import { useEditText } from "@rightkit/document-core/app/useEditText";
import { useMirroredEditText } from "@rightkit/document-core/documents/useMirroredEditText";
import { useSplitResize } from "@rightkit/document-core/documents/useSplitResize";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import { MarkdownSourceDrawer, useMarkdownSourceDrawer } from "../markdown/MarkdownSplitPane";
import { MdxPreviewFrame } from "@rightkit/markdown-editor/documents/mdx/MdxPreviewFrame";
import { DocumentToolbarSearch, type DocumentSearchMode } from "@rightkit/document-core/documents/DocumentToolbarFind";

const noopSubscribeEdits = (_path: string, _cb: () => void) => () => {};
const noopEditSnapshot = (_path: string) => undefined;

interface Props {
  text: string;
  documentPath?: string;
  subscribeEdits?: (path: string, cb: () => void) => () => void;
  getEditSnapshot?: (path: string) => string | undefined;
  devMode?: boolean;
  active?: boolean;
  showSource: boolean;
  /** Wave 2 Read mode: the MDX preview is inherently read-only (sandboxed iframe); the flag only
   *  forces the source drawer closed — the drawer is the editing surface. */
  readOnly?: boolean;
  onChange: (text: string) => void;
  onSave: () => void;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  findBar?: ReactNode;
  findMode?: DocumentSearchMode;
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  /** Close the shared find bar when the rendered/source surface switches. */
  onCloseFind?: () => void;
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void;
  /** Reports every grant minted for this document's compile-time imports + in-preview link clicks,
   *  so the owning tab can revoke them on close. */
  onImportGranted?: (entryPath: string, grantedPath: string) => void;
}

export function MdxSplitPane({
  text,
  documentPath,
  subscribeEdits,
  getEditSnapshot,
  showSource,
  onChange,
  onSave,
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  onCloseFind,
  onOpenPath,
  onImportGranted,
  readOnly = false,
  active = true,
}: Props) {
  const liveText = useEditText(
    subscribeEdits ?? noopSubscribeEdits,
    getEditSnapshot ?? noopEditSnapshot,
    documentPath ?? "",
  );
  const [localText, setLocalText] = useMirroredEditText(liveText, text);
  const handleChange = useCallback(
    (next: string) => {
      setLocalText(next);
      onChange(next);
    },
    [onChange, setLocalText],
  );
  const renderedText = localText;
  const containerRef = useRef<HTMLDivElement>(null);
  const { sourceWidth, setSourceWidth, sourceFullscreen, drawerProps } = useMarkdownSourceDrawer(
    renderedText,
    undefined,
    undefined,
    undefined,
    documentPath,
  );

  // The active surface owns find: the source editor while the drawer is open, else the MDX
  // preview. publish() exposes whichever is active so neither child's register/unregister can
  // clobber the other during the toggle. Same invariant as MarkdownSplitPane.tsx:54-64.
  const previewProviderRef = useRef<SearchProvider | null>(null);
  const sourceProviderRef = useRef<SearchProvider | null>(null);
  // Read mode keeps the drawer closed even if a persisted view asks for "source" — the drawer is
  // the editing surface, and Read never exposes one.
  const drawer = showSource && !readOnly;
  useCloseFindOnSurfaceChange(drawer, !!findMode, onCloseFind);
  const showSourceRef = useRef(drawer);
  showSourceRef.current = drawer;
  const publish = useCallback(() => {
    registerSearchProvider(
      showSourceRef.current ? sourceProviderRef.current : previewProviderRef.current,
    );
  }, [registerSearchProvider]);
  const registerPreviewProvider = useCallback(
    (p: SearchProvider | null) => {
      previewProviderRef.current = p;
      publish();
    },
    [publish],
  );
  const registerSourceProvider = useCallback(
    (p: SearchProvider | null) => {
      sourceProviderRef.current = p;
      publish();
    },
    [publish],
  );
  useEffect(() => {
    publish();
  }, [publish]);
  useEffect(() => () => registerSearchProvider(null), [registerSearchProvider]);

  const startResize = useSplitResize(containerRef, setSourceWidth);

  return (
    <div ref={containerRef} className="flex h-full min-h-0 min-w-0 bg-ink">
      {/* Preview is ALWAYS mounted (never remounts when the source drawer opens/closes). */}
      {!(drawer && sourceFullscreen) && (
        <section className="relative flex min-h-0 min-w-0 flex-1 flex-col">
          {!drawer && findBar ? (
            <div className="no-print vr-chrome vr-document-toolbar vr-document-toolbar-search-layout relative shrink-0 border-b border-line bg-panel-2 text-muted">
              <DocumentToolbarSearch
                findBar={findBar}
                activeMode={findMode}
                onRequestFind={onRequestFind}
                onRequestReplace={undefined}
              />
            </div>
          ) : null}
          <div data-document-scroll className="min-h-0 flex-1 overflow-auto">
            <MdxPreviewFrame
              source={renderedText}
              documentPath={documentPath ?? ""}
              onOpenPath={onOpenPath}
              registerSearchProvider={registerPreviewProvider}
              onImportGranted={onImportGranted}
              active={active}
              printPreview={!drawer}
            />
          </div>
        </section>
      )}

      {drawer && !sourceFullscreen && (
        <div
          onPointerDown={startResize}
          role="separator"
          tabIndex={0}
          aria-orientation="vertical"
          aria-valuemin={30}
          aria-valuenow={sourceWidth}
          aria-label="Resize MDX source drawer"
          className="w-1.5 shrink-0 cursor-col-resize bg-line transition-colors hover:bg-copper/60"
        />
      )}

      {drawer && (
        <MarkdownSourceDrawer
          documentPath={documentPath}
          text={renderedText}
          onChange={handleChange}
          onSave={onSave}
          registerSourceProvider={registerSourceProvider}
          findBar={findBar}
          findMode={findMode}
          onRequestFind={onRequestFind}
          onRequestReplace={onRequestReplace}
          {...drawerProps}
          restoreTitle="Restore MDX split"
          expandTitle="Expand MDX source"
          className={`relative flex min-h-0 min-w-0 flex-col border-l border-line bg-panel ${sourceFullscreen ? "flex-1" : "shrink-0"}`}
        />
      )}
    </div>
  );
}
