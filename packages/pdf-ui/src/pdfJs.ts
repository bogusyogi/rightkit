// pdf.js bootstrap. The worker and wasm decoder locations are host-configurable because they
// depend on the bundler: call `configurePdfJs` once at startup, or rely on the defaults (worker
// resolved with `new URL(..., import.meta.url)`, which Vite/webpack/Rolldown rewrite to an asset).
import * as pdfjs from "pdfjs-dist";

export interface PdfJsConfig {
  /** URL of `pdfjs-dist/build/pdf.worker.min.mjs`. */
  workerSrc?: string;
  /** Directory URL (trailing slash) serving pdf.js's JBIG2/OpenJPEG/QCMS wasm files. Default "/pdfjs/". */
  wasmUrl?: string;
}

let configured: PdfJsConfig = {};

export function configurePdfJs(config: PdfJsConfig): void {
  configured = { ...configured, ...config };
  if (config.workerSrc) pdfjs.GlobalWorkerOptions.workerSrc = config.workerSrc;
}

export function pdfWasmUrl(): string {
  return configured.wasmUrl ?? "/pdfjs/";
}

export function ensurePdfWorker(): void {
  if (pdfjs.GlobalWorkerOptions.workerSrc) return;
  pdfjs.GlobalWorkerOptions.workerSrc = new URL(
    "pdfjs-dist/build/pdf.worker.min.mjs",
    import.meta.url,
  ).href;
}
