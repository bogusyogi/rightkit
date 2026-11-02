import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { ViewMeta } from "../types.js";
import type { OutlineNode, Size } from "./pdfShared.js";
import { acquireResource } from "../../app/resourceCensus.js";
import {
  beginPdfBench,
  getPdfBenchLoadMode,
  markPdfBenchDocumentReady,
  markPdfBenchError,
} from "./pdfBench.js";
import { preparePdfLoading, type PreparedPdfLoading } from "./pdfBenchTransport.js";
import { pdfWasmUrl } from "@rightkit/pdf-ui";

/** Benchmark-only password injection; production & manual unlocks always use the prompt. */
export function getPdfBenchPassword(): string | null {
  if (!import.meta.env?.DEV || import.meta.env?.VITE_PDF_BENCH_AUTOPILOT !== "1") return null;
  const password = import.meta.env?.VITE_PDF_BENCH_PASSWORD as string | undefined;
  return password ? password : null;
}

interface UsePdfDocumentLoaderArgs {
  documentPath: string;
  url?: string;
  initialView?: ViewMeta;
  pdfRef: MutableRefObject<PDFDocumentProxy | null>;
  renderedRef: MutableRefObject<Map<number, () => void>>;
  slotRefs: MutableRefObject<Map<number, HTMLDivElement>>;
  visibleRef: MutableRefObject<Set<number>>;
  restoredRef: MutableRefObject<boolean>;
  reportReadyRef: MutableRefObject<boolean>;
  setCurrent: Dispatch<SetStateAction<number>>;
  setScale: Dispatch<SetStateAction<number>>;
  setRotation: Dispatch<SetStateAction<number>>;
  setFitMode: Dispatch<SetStateAction<"width" | "page" | null>>;
}

export function usePdfDocumentLoader({
  documentPath,
  url,
  initialView,
  pdfRef,
  renderedRef,
  slotRefs,
  visibleRef,
  restoredRef,
  reportReadyRef,
  setCurrent,
  setScale,
  setRotation,
  setFitMode,
}: UsePdfDocumentLoaderArgs) {
  const [pdfDoc, setPdfDoc] = useState<PDFDocumentProxy | null>(null);
  const [numPages, setNumPages] = useState(0);
  const [base, setBase] = useState<Size | null>(null);
  const [pageSizes, setPageSizes] = useState<Map<number, Size>>(new Map());
  const [error, setError] = useState<string | null>(null);
  const [renderErrors, setRenderErrors] = useState<Map<number, string>>(new Map());
  const [pwPrompt, setPwPrompt] = useState<{
    retry: boolean;
    onSubmit: (pw: string) => void;
    onCancel: () => void;
  } | null>(null);
  const [outline, setOutline] = useState<OutlineNode[] | null>(null);

  // The pdf.js loading task currently backing `pdfDoc`/`pdfRef.current` — either this effect's own
  // initial-load task, or a post-commit `replaceDocument` task (§10.3 reload after Save). Whichever
  // task is here must only be destroyed once React has committed the DIFFERENT `pdfDoc` that
  // superseded it (the promote effect below) — destroying a task while page layers still hold the
  // document it produced is what previously crashed the viewer to its ErrorBoundary.
  const activeTaskRef = useRef<ReturnType<typeof pdfjs.getDocument> | null>(null);
  // Set by `replaceDocument`, consumed by the promote effect once `pdfDoc` actually changes.
  const pendingReplacementTaskRef = useRef<ReturnType<typeof pdfjs.getDocument> | null>(null);

  const replaceDocument = useCallback(
    (doc: PDFDocumentProxy, task: ReturnType<typeof pdfjs.getDocument>) => {
      pdfRef.current = doc;
      pendingReplacementTaskRef.current = task;
      setPdfDoc(doc);
    },
    [pdfRef],
  );

  // Promotes a pending replacement to "active" — and disposes whatever it superseded — only after
  // React has committed `pdfDoc` to the new document. Also runs (as a no-op) after the ordinary
  // initial load's `setPdfDoc`, since `pendingReplacementTaskRef` is still null at that point.
  // `pdfDoc` is the intentional trigger (the effect must run after React commits the new document),
  // not a value read inside the effect body.
  // biome-ignore lint/correctness/useExhaustiveDependencies: see comment above.
  useEffect(() => {
    const pending = pendingReplacementTaskRef.current;
    if (!pending || pending === activeTaskRef.current) return;
    const stale = activeTaskRef.current;
    activeTaskRef.current = pending;
    pendingReplacementTaskRef.current = null;
    if (stale) void stale.destroy();
  }, [pdfDoc]);

  useEffect(() => {
    if (!url) return;
    let cancelled = false;
    let passwordCancelled = false;
    renderedRef.current.forEach((cleanup) => cleanup());
    renderedRef.current.clear();
    slotRefs.current.clear();
    visibleRef.current.clear();
    pdfRef.current = null;
    restoredRef.current = false;
    reportReadyRef.current = false;
    setError(null);
    setRenderErrors(new Map());
    setNumPages(0);
    setBase(null);
    setPageSizes(new Map());
    setCurrent(1);
    setPwPrompt(null);
    setOutline(null);
    setPdfDoc(null);

    const controller = new AbortController();
    let task: ReturnType<typeof pdfjs.getDocument> | null = null;
    let preparedLoad: PreparedPdfLoading | null = null;
    let loadedDoc: PDFDocumentProxy | null = null;
    // One document proxy + its worker per open PDF. Released in the effect cleanup below so a
    // cold-unmounted PDF pane provably stops counting against resident cost.
    let releaseDocumentCensus: (() => void) | null = null;
    let releaseWorkerCensus: (() => void) | null = null;
    let finalized = false;
    const finalize = () => {
      if (finalized) return;
      finalized = true;
      controller.abort();
      if (preparedLoad) preparedLoad.dispose();
      else if (task) void task.destroy();
      if (activeTaskRef.current === task) activeTaskRef.current = null;
      if (pendingReplacementTaskRef.current === task) pendingReplacementTaskRef.current = null;
      releaseDocumentCensus?.();
      releaseDocumentCensus = null;
      releaseWorkerCensus?.();
      releaseWorkerCensus = null;
      if (pdfRef.current && pdfRef.current === loadedDoc) pdfRef.current = null;
    };
    const load = async () => {
      const mode = getPdfBenchLoadMode();
      await beginPdfBench(documentPath, pdfjs.version, mode);
      if (cancelled) {
        const error = new Error("PDF load cancelled");
        error.name = "AbortError";
        throw error;
      }
      preparedLoad = await preparePdfLoading({
        url,
        mode,
        signal: controller.signal,
        wasmUrl: pdfWasmUrl(),
      });
      task = preparedLoad.task;
      releaseWorkerCensus = acquireResource({
        kind: "pdf-worker",
        documentPath,
        format: "pdf",
      });
      task.onPassword = (updatePassword: (p: string) => void, reason: number) => {
        const benchPassword = getPdfBenchPassword();
        if (benchPassword) {
          updatePassword(benchPassword);
          return;
        }
        setPwPrompt({
          retry: reason === 2,
          onSubmit: (pw: string) => {
            setPwPrompt(null);
            updatePassword(pw);
          },
          onCancel: () => {
            passwordCancelled = true;
            setPwPrompt(null);
            setError("Password required to open this PDF.");
            finalize();
          },
        });
      };
      return preparedLoad.promise;
    };

    load()
      .then(async (doc) => {
        if (cancelled || finalized) return;
        markPdfBenchDocumentReady(documentPath);
        setPwPrompt(null);
        loadedDoc = doc;
        pdfRef.current = doc;
        activeTaskRef.current = task;
        releaseDocumentCensus = acquireResource({
          kind: "pdf-document",
          documentPath,
          format: "pdf",
        });
        setPdfDoc(doc);
        setNumPages(doc.numPages);
        doc
          .getOutline()
          .then((o) => {
            if (!cancelled && !finalized) setOutline((o as OutlineNode[] | null) ?? []);
          })
          .catch(() => {
            if (!cancelled && !finalized) setOutline([]);
          });
        const first = await doc.getPage(1);
        const v = first.getViewport({ scale: 1 });
        if (!cancelled && !finalized) {
          setBase({ w: v.width, h: v.height });
          setRotation((((initialView?.rotation ?? 0) % 360) + 360) % 360);
          if (initialView?.fit) setFitMode(initialView.fit);
          else if (initialView?.scale) {
            setScale(initialView.scale);
            setFitMode(null);
          }
        }
      })
      .catch((e) => {
        if (!cancelled && !passwordCancelled && (e as { name?: string })?.name !== "AbortError") {
          finalize();
          if (loadedDoc) {
            pdfRef.current = null;
            setPdfDoc(null);
            setNumPages(0);
          }
          markPdfBenchError(documentPath, e);
          setError(String((e as { message?: unknown })?.message ?? e));
        }
      });

    return () => {
      cancelled = true;
      renderedRef.current.forEach((cleanup) => cleanup());
      renderedRef.current.clear();
      finalize();
      // A post-commit `replaceDocument` call may have swapped in a newer task after the initial
      // load above finished; dispose it too (and any not-yet-promoted pending one) so a document
      // switch/unmount can't leak its pdf.js worker.
      if (activeTaskRef.current && activeTaskRef.current !== task) {
        void activeTaskRef.current.destroy();
      }
      if (
        pendingReplacementTaskRef.current &&
        pendingReplacementTaskRef.current !== activeTaskRef.current
      ) {
        void pendingReplacementTaskRef.current.destroy();
      }
      pdfRef.current = null;
    };
  }, [
    documentPath,
    url,
    slotRefs.current.clear,
    setScale,
    visibleRef.current.clear,
    setRotation,
    setFitMode,
    setCurrent,
    restoredRef,
    reportReadyRef,
    pdfRef,
    renderedRef.current.forEach,
    initialView?.scale,
    renderedRef.current.clear,
    initialView?.rotation,
    initialView?.fit,
  ]);

  return {
    pdfDoc,
    numPages,
    base,
    pageSizes,
    setPageSizes,
    error,
    renderErrors,
    setRenderErrors,
    pwPrompt,
    outline,
    replaceDocument,
  };
}
