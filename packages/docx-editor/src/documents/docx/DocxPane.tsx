import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { DocxEditor, type DocxEditorRef, type DocxHostToolbarContext } from "./eigenpalReact.js";
import { setGoogleFontsEnabled } from "@eigenpal/docx-editor-core";
import { resolveAccentColors, resolveTheme, useSettings } from "@rightkit/document-core/settings/settings";
import { buildEigenpalThemeCss } from "./eigenpalTheme.js";
import { HostDocxToolbar } from "./HostDocxToolbar.js";
import type { DocxMenuApi } from "./menuApi.js";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import { createDocxFindController, type DocxFindController } from "./docxFind.js";
import { detectBlankDocxRender } from "./docxFallback.js";
import { base64ToBuffer } from "./blankDocx.js";
import { docxPrintMarkup } from "./docxPrint.js";
import { printHtmlMarkup } from "@rightkit/document-core/app/documentPrinting";
import { assertDocxWithinBudget } from "./docxBudget.js";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { toast } from "@rightkit/platform-ui/headless";
import { acquireResource, updateResource } from "@rightkit/document-core/app/resourceCensus";
import { hostAppName } from "../../appName.js";

// the host app is local-first: DOCX uses embedded/system fonts plus local CSS fallbacks. Disabling
// implicit Google lookup prevents invalid requests for Office-only families such as Calibri Light.
setGoogleFontsEnabled(false);

interface Props {
  /** Canonical tab/document identity used by resource census; never use the broker asset URL. */
  documentPath: string;
  /** Asset-protocol URL for the .docx; bytes are fetched from disk, not passed as base64. */
  url?: string;
  /** Base64 DOCX bytes for never-saved documents. */
  initialBase64?: string;
  /** Fires when the user saves inside the editor (File→Save / Ctrl+S) → write to disk. */
  onSave: (buffer: ArrayBuffer) => void;
  /** Fires on edits so the app can track unsaved state (close warning). */
  onChange: () => void;
  /** Registers a programmatic save() so the close dialog's "Save" works for docx. */
  registerSave: (fn: (() => Promise<ArrayBuffer | null>) | null) => void;
  /** Lifts the docx command surface up so the app's File/Format/Insert/Help menus can drive it. */
  registerMenu?: (api: DocxMenuApi | null) => void;
  /** Register the DOCX SearchProvider so DOCX uses the shared FindBar (not the vendor dialog). */
  registerSearchProvider?: (provider: SearchProvider | null) => void;
  /** Shared FindBar element, floated top-right inside the DOCX page area (below the toolbar). */
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  /** Wave 2 Read mode — drives the vendor's "viewing" editing mode: the document becomes truly
   *  read-only while the the host app toolbar bridge stays mounted for viewing controls. */
  readOnly?: boolean;
}

/** Editable DOCX pane (round-trip via Eigenpal's Apache-2.0 engine, client-side).
 *  the host app owns the visible toolbar; Eigenpal owns the DOCX engine/canvas. */
export function DocxPane({
  documentPath,
  url,
  initialBase64,
  onSave,
  onChange,
  registerSave,
  registerMenu,
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  readOnly = false,
}: Props) {
  const ref = useRef<DocxEditorRef>(null);
  const paneRef = useRef<HTMLDivElement | null>(null);
  const [buffer, setBuffer] = useState<ArrayBuffer | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [textFallback, setTextFallback] = useState<string | null>(null);
  const { settings } = useSettings();
  const findControllerRef = useRef<DocxFindController | null>(null);
  const fallbackTimerRef = useRef<number | null>(null);
  // Outline panel: vendor-rendered (headings nav + scroll-to-heading), but driven by OUR toolbar
  // toggle and with the vendor's floating button hidden — so it's docked like the PDF outline, not
  // a floating icon overlapping the page. `showOutline` is a live-controlled vendor prop.
  const [outlineOpen, setOutlineOpen] = useState(false);

  // One EigenPal editor per live DOCX pane. DOCX has no external owner for its in-editor OOXML
  // state yet, so a dirty pane reports as pinned — that is exactly the residency the pack defers
  // to the P2 hibernation measurement, and it should stay visible until then.
  const censusRef = useRef<(() => void) | null>(null);
  useEffect(() => {
    const release = acquireResource({
      kind: "docx-editor",
      documentPath,
      format: "docx",
    });
    censusRef.current = release;
    return () => {
      release();
      censusRef.current = null;
    };
  }, [documentPath]);

  // Latest bridge context, captured each time the editor renders its toolbar slot.
  const ctxRef = useRef<DocxHostToolbarContext | null>(null);
  // Stable menu API (created once) — lazily reads the live context at click time, so the
  // app-level File/Format/Insert/Help menus never hold a stale handler or re-render the app.
  const menuApi = useMemo<DocxMenuApi>(
    () => ({
      // In-place save via the editor's imperative ref (handleSave → onSave → write to disk).
      // NOT ctx.save: the host-toolbar ctx.save maps to the vendor's "download document" flow, which
      // also drops a redundant .docx into the browser Downloads folder on every Ctrl+S.
      save: () => void ref.current?.save(),
      // NOT ctx.print: the vendor's own print path calls `window.open('', '_blank')` to build a
      // detached print preview window, which Tauri's WKWebView returns null/unusable for — a silent
      // no-op. Clone the rendered pages into the app's own print host and use the native print
      // command instead (documentPrinting.ts), which is the one path confirmed to work here.
      // Deliberately NO fallback to ctxRef.current.print(): that path cannot work in this shell, and
      // falling back to it is what made a missed selector look like "print does nothing at all".
      // Selector list is ordered most- to least-specific; the pane itself is the last resort so the
      // user always gets either a print sheet or an explicit error.
      print: () => {
        const pane = paneRef.current;
        const source =
          pane?.querySelector(".paged-editor__pages") ??
          pane?.querySelector("[class*='paged-editor']") ??
          pane?.querySelector(".layout-page")?.parentElement ??
          pane;
        if (!source) {
          logError("print:failed:docx-no-pages");
          toast("Couldn't open print dialog", "error");
          return;
        }
        void printHtmlMarkup(docxPrintMarkup(source))
          .then((printed) => {
            if (printed) return;
            logError("print:failed:docx");
            toast("Couldn't open print dialog", "error");
          })
          .catch(() => {
            logError("print:failed:docx");
            toast("Couldn't open print dialog", "error");
          });
      },
      pageSetup: () => ctxRef.current?.pageSetup(),
      format: (a) => ctxRef.current?.format(a),
      insertTable: (r, c) => ctxRef.current?.insertTable(r, c),
      insertImage: () => ctxRef.current?.insertImage(),
      insertPageBreak: () => ctxRef.current?.insertPageBreak(),
      insertTOC: () => ctxRef.current?.insertTOC(),
    }),
    [],
  );
  useEffect(() => {
    registerMenu?.(menuApi);
    return () => registerMenu?.(null);
  }, [registerMenu, menuApi]);

  const onWheelZoom = useCallback((event: ReactWheelEvent<HTMLDivElement>) => {
    if (!(event.ctrlKey || event.metaKey)) return;
    const ctx = ctxRef.current;
    if (!ctx) return;
    event.preventDefault();
    event.stopPropagation();
    const next = Math.min(
      2,
      Math.max(0.5, Number((ctx.zoom + (event.deltaY < 0 ? 0.1 : -0.1)).toFixed(2))),
    );
    if (next !== ctx.zoom) ctx.setZoom(next);
  }, []);

  // Read mode must not advertise replace: the registered provider is re-published with
  // `canReplace` flipped whenever the mode changes (same pattern as the source editor).
  const readOnlyRef = useRef(readOnly);
  const providerRef = useRef<SearchProvider | null>(null);
  useEffect(() => {
    readOnlyRef.current = readOnly;
    const current = providerRef.current;
    if (!current) return;
    const next = { ...current, canReplace: !readOnly };
    providerRef.current = next;
    registerSearchProvider?.(next);
  }, [readOnly, registerSearchProvider]);

  // When the editor's ProseMirror view is ready, build a the host app-owned Find controller over it
  // and register a "docx" SearchProvider — so DOCX find/replace runs through the shared FindBar.
  // The vendor's own Ctrl+F dialog is disabled (disableFindReplaceShortcuts) so there's one surface.
  const onEditorViewReady = useCallback(
    (
      view: Parameters<
        NonNullable<React.ComponentProps<typeof DocxEditor>["onEditorViewReady"]>
      >[0],
    ) => {
      const controller = createDocxFindController(view, (pos) =>
        ref.current?.scrollToPosition(pos),
      );
      findControllerRef.current = controller;
      const provider: SearchProvider = {
        kind: "docx",
        canReplace: !readOnlyRef.current,
        supportsMatchCase: true,
        supportsWholeWord: true,
        supportsRegex: true,
        find: (q, o) => controller.find(q, o),
        next: () => controller.next(),
        prev: () => controller.previous(),
        replace: (r) => controller.replace(r),
        replaceAll: (r) => controller.replaceAll(r),
        close: () => controller.clear(),
      };
      providerRef.current = provider;
      registerSearchProvider?.(provider);
      const sourceText = view.state.doc.textContent;
      if (fallbackTimerRef.current != null) window.clearTimeout(fallbackTimerRef.current);
      const checkFallback = (attempt: number) => {
        fallbackTimerRef.current = null;
        const fallback = detectBlankDocxRender(paneRef.current, sourceText);
        const hasPages = !!paneRef.current?.querySelector(".layout-page");
        if (fallback || hasPages || attempt >= 6) {
          // Blank-render evidence: record what the vendor paint path produced whenever the
          // fallback engages or no page ever painted (vendor swallows layout errors).
          if (fallback || !hasPages) {
            logError("docx:blank-render");
          }
          setTextFallback(fallback);
          return;
        }
        fallbackTimerRef.current = window.setTimeout(() => checkFallback(attempt + 1), 500);
      };
      fallbackTimerRef.current = window.setTimeout(() => checkFallback(1), 1200);
    },
    [registerSearchProvider],
  );
  useEffect(
    () => () => {
      if (fallbackTimerRef.current != null) window.clearTimeout(fallbackTimerRef.current);
      findControllerRef.current?.clear();
      providerRef.current = null;
      registerSearchProvider?.(null);
    },
    [registerSearchProvider],
  );

  const epThemeCss = useMemo(() => {
    const theme = resolveTheme(settings);
    return buildEigenpalThemeCss(theme.tokens, resolveAccentColors(settings).primary, theme.mode);
  }, [settings]);

  // Stream the .docx bytes from the asset URL (broker-scoped) instead of decoding base64 IPC.
  useEffect(() => {
    if (initialBase64) {
      try {
        const decoded = base64ToBuffer(initialBase64);
        assertDocxWithinBudget(decoded);
        setBuffer(decoded);
        setLoadError(null);
      } catch (error) {
        setBuffer(null);
        setLoadError(
          `could not load Word document (${error instanceof Error ? error.message : String(error)})`,
        );
      }
      setTextFallback(null);
      return;
    }
    if (!url) {
      setBuffer(null);
      setLoadError(null);
      return;
    }
    let cancelled = false;
    setLoadError(null);
    setTextFallback(null);
    if (fallbackTimerRef.current != null) {
      window.clearTimeout(fallbackTimerRef.current);
      fallbackTimerRef.current = null;
    }
    fetch(url)
      .then((r) => {
        if (!r.ok) throw new Error(`could not load Word document (${r.status})`);
        return r.arrayBuffer();
      })
      .then((b) => {
        if (cancelled) return;
        assertDocxWithinBudget(b);
        setBuffer(b);
      })
      .catch((e) => {
        if (!cancelled) {
          setBuffer(null);
          setLoadError(String(e?.message ?? e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [url, initialBase64]);

  useEffect(() => {
    registerSave(async () => {
      const out = (await ref.current?.save()) as unknown;
      if (!out) return null;
      if (out instanceof Blob) return await out.arrayBuffer();
      if (out instanceof ArrayBuffer) return out;
      // Slice to the view's own range — a pooled/sub-view Uint8Array's .buffer can be larger than
      // the document bytes, which would otherwise write garbage past the end of the file.
      if (out instanceof Uint8Array)
        return out.buffer.slice(out.byteOffset, out.byteOffset + out.byteLength) as ArrayBuffer;
      return null;
    });
    return () => registerSave(null);
  }, [registerSave]);

  if (!url && !initialBase64) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-2">
        Open via the desktop app.
      </div>
    );
  }
  if (loadError) {
    return (
      <div className="flex h-full items-center justify-center px-6 text-center text-sm text-copper-hi">
        DOCX error: {loadError}
      </div>
    );
  }
  if (!buffer) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-2">Loading…</div>
    );
  }

  return (
    <div className="relative h-full w-full">
      {/* The tool bar is docked in its own row inside the vendor layout (renderToolbar), so the
          paged viewport starts below it in both modes. */}
      <div className="vr-docx h-full w-full overflow-auto" onWheel={onWheelZoom}>
        <style>{epThemeCss}</style>
        <div ref={paneRef} className="h-full w-full">
          <DocxEditor
            ref={ref}
            documentBuffer={buffer}
            mode={readOnly ? "viewing" : "editing"}
            onSave={onSave}
            onChange={() => {
              if (censusRef.current)
                updateResource(censusRef.current, { dirty: true, stateExternalized: false });
              onChange();
            }}
            showOutline={outlineOpen}
            showOutlineButton={false}
            disableFindReplaceShortcuts
            onEditorViewReady={onEditorViewReady}
            renderToolbar={(context) => {
              ctxRef.current = context;
              return (
                <HostDocxToolbar
                  context={context}
                  readOnly={readOnly}
                  outlineOpen={outlineOpen}
                  onToggleOutline={() => setOutlineOpen((o) => !o)}
                  findBar={findBar}
                  findMode={findMode}
                  onRequestFind={onRequestFind}
                  onRequestReplace={onRequestReplace}
                />
              );
            }}
          />
        </div>
        {textFallback && <DocxTextFallback text={textFallback} />}
      </div>
    </div>
  );
}

function DocxTextFallback({ text }: { text: string }) {
  return (
    <div className="absolute left-6 top-24 z-30 max-h-72 w-[min(720px,calc(100%-3rem))] overflow-auto rounded border border-copper/35 bg-panel-2/95 p-4 shadow-xl">
      <div className="mb-2 text-chrome-xs uppercase tracking-wide text-copper-hi">
        DOCX text fallback
      </div>
      <p className="mb-3 max-w-2xl text-xs leading-5 text-muted">
        {hostAppName()} could read text from this DOCX, but the editable page renderer produced a blank
        page. Showing extracted text read-only so the document is not mistaken for empty.
      </p>
      <pre className="whitespace-pre-wrap font-sans text-sm leading-6 text-paper">{text}</pre>
    </div>
  );
}
