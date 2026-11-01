import { useCallback, useEffect, useRef, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import {
  isBackendUnavailable,
  isProbablyScannedPage,
  isTextlessPage,
  ocrBackendMissing,
  ocrPage,
  renderPageToPng,
  type OcrLine,
} from "./pdfOcr";
import { viewportRotation } from "./pdfShared";
import { logError, logPerf } from "../../state/diagnostics";

export type PdfOcrStatus = "idle" | "running" | "done" | "error";

export interface PdfOcrPageState {
  status: PdfOcrStatus;
  error?: string;
}

export interface PdfOcrDocumentState {
  status: PdfOcrStatus;
  completed: number;
  total: number;
  scanned: number;
  skipped: number;
  failed: number;
  currentPage?: number;
}

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return typeof error === "string" ? error : String(error);
}

export function usePdfOcr({
  documentId,
  pdfDoc,
  pdfRef,
  current,
  rotation,
  setPageText,
  invalidatePageText,
  rehighlight,
}: {
  documentId: string;
  pdfDoc: PDFDocumentProxy | null;
  pdfRef: React.MutableRefObject<PDFDocumentProxy | null>;
  current: number;
  rotation: number;
  setPageText: (page: number, text: string) => void;
  invalidatePageText: (page: number) => void;
  rehighlight: () => void;
}) {
  const [scannedPages, setScannedPages] = useState<Map<number, boolean>>(new Map());
  const [pageStates, setPageStates] = useState<Map<number, PdfOcrPageState>>(new Map());
  const [linesByPage, setLinesByPage] = useState<Map<number, OcrLine[]>>(new Map());
  const [currentPageScanned, setCurrentPageScanned] = useState(false);
  const [documentState, setDocumentState] = useState<PdfOcrDocumentState>({
    status: "idle",
    completed: 0,
    total: 0,
    scanned: 0,
    skipped: 0,
    failed: 0,
  });

  const generationRef = useRef(0);
  const scanRequestRef = useRef(0);
  const pageRunRef = useRef<Map<number, number>>(new Map());
  const documentRunRef = useRef<number | null>(null);
  const previousRotationRef = useRef(rotation);
  const linesByPageRef = useRef(linesByPage);
  const scannedPagesRef = useRef(scannedPages);
  const pageStatesRef = useRef(pageStates);
  const documentStateRef = useRef(documentState);
  linesByPageRef.current = linesByPage;
  scannedPagesRef.current = scannedPages;
  pageStatesRef.current = pageStates;
  documentStateRef.current = documentState;

  // documentId is the lifecycle boundary; reset OCR state when it changes.
  // biome-ignore lint/correctness/useExhaustiveDependencies: documentId intentionally marks document lifecycle.
  useEffect(() => {
    generationRef.current += 1;
    scanRequestRef.current += 1;
    pageRunRef.current.clear();
    documentRunRef.current = null;
    setScannedPages(new Map());
    setPageStates(new Map());
    setLinesByPage(new Map());
    setCurrentPageScanned(false);
    setDocumentState({
      status: "idle",
      completed: 0,
      total: 0,
      scanned: 0,
      skipped: 0,
      failed: 0,
    });

    return () => {
      generationRef.current += 1;
      scanRequestRef.current += 1;
      pageRunRef.current.clear();
      documentRunRef.current = null;
    };
  }, [documentId]);

  // Geometry from a previous rotation is no longer valid. Do not transform it: the backend's
  // normalized coordinates describe the raster orientation, so recognition must run again.
  useEffect(() => {
    if (previousRotationRef.current === rotation) return;
    previousRotationRef.current = rotation;
    generationRef.current += 1;
    scanRequestRef.current += 1;
    pageRunRef.current.clear();
    documentRunRef.current = null;
    const recognizedPages = [...linesByPageRef.current.keys()];
    for (const page of recognizedPages) invalidatePageText(page);
    if (recognizedPages.length > 0) {
      setLinesByPage((previous) => {
        const next = new Map(previous);
        for (const page of recognizedPages) next.delete(page);
        return next;
      });
    }
    setPageStates((previous) => {
      const next = new Map(previous);
      for (const [page, state] of next) {
        if (state.status === "done" || state.status === "running") {
          next.set(page, { status: "idle" });
        }
      }
      return next;
    });
    setDocumentState({ status: "idle", completed: 0, total: 0, scanned: 0, skipped: 0, failed: 0 });
  }, [invalidatePageText, rotation]);

  // Browsing detection reads only streamed text. Image/operator inspection is deferred until an
  // explicit OCR request, so opening/changing pages never decodes a page just to show a button.
  useEffect(() => {
    if (!pdfDoc || current < 1 || current > pdfDoc.numPages) {
      setCurrentPageScanned(false);
      return;
    }
    const known = scannedPagesRef.current.get(current);
    if (known !== undefined) {
      setCurrentPageScanned(known);
      return;
    }
    const request = ++scanRequestRef.current;
    let cancelled = false;
    void pdfDoc
      .getPage(current)
      .then((page) => isTextlessPage(page))
      .then((textless) => {
        if (cancelled || request !== scanRequestRef.current) return;
        setScannedPages((previous) => {
          const next = new Map(previous);
          next.set(current, textless);
          return next;
        });
        setCurrentPageScanned(textless);
      })
      .catch(() => {
        if (!cancelled && request === scanRequestRef.current) setCurrentPageScanned(false);
      });
    return () => {
      cancelled = true;
    };
  }, [current, pdfDoc]);

  const setState = useCallback((page: number, state: PdfOcrPageState) => {
    setPageStates((previous) => {
      const next = new Map(previous);
      next.set(page, state);
      return next;
    });
  }, []);

  const isCurrentGeneration = useCallback(
    (generation: number, pdf: PDFDocumentProxy) =>
      generation === generationRef.current && pdfRef.current === pdf,
    [pdfRef],
  );

  const recognizePage = useCallback(
    async (
      pageNumber: number,
      pdf: PDFDocumentProxy,
      generation: number,
      pageOverride?: Awaited<ReturnType<PDFDocumentProxy["getPage"]>>,
    ): Promise<"done" | "failed" | "cancelled"> => {
      const startedAt = performance.now();
      setState(pageNumber, { status: "running" });
      try {
        const page = pageOverride ?? (await pdf.getPage(pageNumber));
        if (!isCurrentGeneration(generation, pdf)) return "cancelled";
        const png = await renderPageToPng(page, 300, viewportRotation(page.rotate, rotation));
        if (!isCurrentGeneration(generation, pdf)) return "cancelled";
        const lines = await ocrPage(new Uint8Array(await png.arrayBuffer()));
        if (!isCurrentGeneration(generation, pdf)) return "cancelled";
        setLinesByPage((previous) => new Map(previous).set(pageNumber, lines));
        setState(pageNumber, { status: "done" });
        setPageText(pageNumber, lines.map((line) => line.text).join(" "));
        logPerf("pdf:ocr-page", startedAt);
        rehighlight();
        return "done";
      } catch (error) {
        if (!isCurrentGeneration(generation, pdf)) return "cancelled";
        setState(pageNumber, { status: "error", error: errorMessage(error) });
        logError("pdf:ocr-page-failed");
        return "failed";
      }
    },
    [isCurrentGeneration, rehighlight, rotation, setPageText, setState],
  );

  const recognizeCurrentPage = useCallback(
    async (inspectEligibility = false) => {
      const pageNumber = current;
      const pdf = pdfRef.current ?? pdfDoc;
      if (
        !pdf ||
        (!currentPageScanned && !inspectEligibility) ||
        pageStatesRef.current.get(pageNumber)?.status === "running" ||
        pageRunRef.current.size > 0 ||
        documentRunRef.current !== null
      ) {
        return;
      }

      const generation = generationRef.current;
      pageRunRef.current.set(pageNumber, generation);
      setState(pageNumber, { status: "running" });
      try {
        const page = await pdf.getPage(pageNumber);
        if (!isCurrentGeneration(generation, pdf)) return;
        const scanned = await isProbablyScannedPage(page);
        if (!isCurrentGeneration(generation, pdf)) return;
        setScannedPages((previous) => new Map(previous).set(pageNumber, scanned));
        setCurrentPageScanned(scanned);
        if (!scanned) {
          setState(pageNumber, { status: "idle" });
          return;
        }
        await recognizePage(pageNumber, pdf, generation, page);
      } catch {
        if (!isCurrentGeneration(generation, pdf)) return;
        setState(pageNumber, { status: "error", error: "Unable to inspect page for OCR." });
        logError("pdf:ocr-page-failed");
      } finally {
        if (pageRunRef.current.get(pageNumber) === generation)
          pageRunRef.current.delete(pageNumber);
      }
    },
    [current, currentPageScanned, isCurrentGeneration, pdfDoc, pdfRef, recognizePage, setState],
  );

  const recognizeDocument = useCallback(async () => {
    const pdf = pdfRef.current ?? pdfDoc;
    if (
      !pdf ||
      documentStateRef.current.status === "running" ||
      documentRunRef.current !== null ||
      pageRunRef.current.size > 0
    ) {
      return;
    }

    const generation = generationRef.current;
    const total = pdf.numPages;
    documentRunRef.current = generation;
    setDocumentState({
      status: "running",
      completed: 0,
      total,
      scanned: 0,
      skipped: 0,
      failed: 0,
      currentPage: 1,
    });

    const startedAt = performance.now();
    let failedCount = 0;
    try {
      for (let pageNumber = 1; pageNumber <= total; pageNumber += 1) {
        if (!isCurrentGeneration(generation, pdf)) return;
        const previous = pageStatesRef.current.get(pageNumber);
        try {
          const page = await pdf.getPage(pageNumber);
          const scanned = await isProbablyScannedPage(page);
          if (!isCurrentGeneration(generation, pdf)) return;
          setScannedPages((pages) => new Map(pages).set(pageNumber, scanned));

          if (scanned) {
            setDocumentState((state) => ({
              ...state,
              scanned: state.scanned + 1,
              currentPage: pageNumber,
            }));
          }
          if (!scanned || previous?.status === "done") {
            setDocumentState((state) => ({
              ...state,
              completed: state.completed + 1,
              skipped: state.skipped + 1,
              currentPage: pageNumber,
            }));
            continue;
          }

          const outcome = await recognizePage(pageNumber, pdf, generation, page);
          if (outcome === "cancelled") return;
          if (outcome === "failed") failedCount += 1;
          setDocumentState((state) => ({
            ...state,
            completed: state.completed + 1,
            failed: state.failed + (outcome === "failed" ? 1 : 0),
            currentPage: pageNumber,
          }));
        } catch {
          if (!isCurrentGeneration(generation, pdf)) return;
          failedCount += 1;
          setState(pageNumber, { status: "error", error: "Unable to inspect page for OCR." });
          logError("pdf:ocr-page-failed");
          setDocumentState((state) => ({
            ...state,
            completed: state.completed + 1,
            failed: state.failed + 1,
            currentPage: pageNumber,
          }));
        }
      }
      if (!isCurrentGeneration(generation, pdf)) return;
      setDocumentState((state) => ({ ...state, status: failedCount > 0 ? "error" : "done" }));
      logPerf("pdf:ocr-document", startedAt);
    } catch {
      if (isCurrentGeneration(generation, pdf)) {
        setDocumentState((state) => ({ ...state, status: "error" }));
        setState(current, {
          status: "error",
          error: "Text recognition is unavailable. Try again.",
        });
      }
    } finally {
      if (documentRunRef.current === generation) documentRunRef.current = null;
    }
  }, [current, isCurrentGeneration, pdfDoc, pdfRef, recognizePage, setState]);

  // State updates commit the OCR layer after recognition returns. Re-run once the lines are
  // mounted so a query that was already committed can mark the newly-rendered spans.
  useEffect(() => {
    if (linesByPage.size > 0) rehighlight();
  }, [linesByPage, rehighlight]);

  // Ask the backend only once a scanned page is in view, which is the first moment OCR would be
  // offered: on macOS the probe runs a recognition canary that is costly on a cold model cache.
  // Until it answers, and whenever it cannot answer, OCR stays offered.
  const [ocrMissing, setOcrMissing] = useState(false);
  useEffect(() => {
    if (!currentPageScanned) return;
    let cancelled = false;
    void ocrBackendMissing().then((missing) => {
      if (!cancelled && missing) setOcrMissing(true);
    });
    return () => {
      cancelled = true;
    };
  }, [currentPageScanned]);

  const currentPageState = pageStates.get(current) ?? { status: "idle" as const };
  const currentError = currentPageState.status === "error" ? currentPageState.error : undefined;
  return {
    // The backend probe decides whether OCR is offered at all; PdfToolbar hides the controls when
    // it is false. Recognition failures on the requested page are still reported from that page.
    ocrAvailable: !ocrMissing,
    availabilityResolved: true,
    currentPageScanned,
    currentPageState,
    currentError,
    currentErrorIsBackendUnavailable: currentError ? isBackendUnavailable(currentError) : false,
    linesByPage,
    pageStates,
    recognizeCurrentPage,
    documentOcrState: documentState,
    recognizeDocument,
  };
}
