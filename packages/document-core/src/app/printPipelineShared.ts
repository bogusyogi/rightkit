import { printCurrentWebview } from "../state/tauriBridge";
import { isWindows } from "../platform";
import { openPrintPreviewWindow } from "./nativePrintPreview";

const PRINT_HOST_ATTR = "data-vr-print-host";
const PRINT_ACTIVE_ATTR = "data-vr-print-active";
const PRINT_IMAGE_LOAD_TIMEOUT_MS = 30_000;
let activePrintCleanup: (() => void) | null = null;

async function waitForPrintImage(img: HTMLImageElement, src: string): Promise<void> {
  let resolveLoad!: () => void;
  let rejectLoad!: (error: Error) => void;
  const loaded = new Promise<void>((resolve, reject) => {
    resolveLoad = resolve;
    rejectLoad = reject;
  });
  void loaded.catch(() => undefined);
  // Install handlers before assigning src so an already-failed image cannot leave fallback waiting
  // forever after decode() rejects.
  img.onload = resolveLoad;
  img.onerror = () => rejectLoad(new Error("Print image failed to load"));
  img.src = src;

  let timer: number | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = window.setTimeout(
      () => reject(new Error("Print image load timed out")),
      PRINT_IMAGE_LOAD_TIMEOUT_MS,
    );
  });
  try {
    if (typeof img.decode === "function") {
      try {
        await Promise.race([img.decode(), timeout]);
        return;
      } catch {
        // Decode is unavailable or failed; bounded load/error fallback below preserves existing
        // browser behavior while ensuring broken images cannot hold print forever.
      }
    }
    await Promise.race([loaded, timeout]);
  } finally {
    window.clearTimeout(timer);
    img.onload = null;
    img.onerror = null;
  }
}

/** Mount arbitrary content in a body-level, print-only host, then invoke the app's native print
 *  command. The host and the rest of the app are toggled via [data-vr-print-active] in
 *  src/styles/index.css so this never touches the normal (non-print) native path used by
 *  md/txt/source/image, which prints the live app tree directly and must be left alone. */
export async function printHost(fill: (host: HTMLDivElement) => unknown): Promise<boolean> {
  // Windows WebView2 does not reliably emit `afterprint` when its native dialog closes. Replace
  // any surviving host before mounting the next format so a later print can never capture stale
  // HTML/PDF/Markdown content.
  activePrintCleanup?.();

  const host = document.createElement("div");
  host.setAttribute(PRINT_HOST_ATTR, "");
  document.body.appendChild(host);
  document.body.setAttribute(PRINT_ACTIVE_ATTR, "");
  let cleaned = false;
  let fallback: number | undefined;
  let onAfterPrint: (() => void) | undefined;
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    if (onAfterPrint) window.removeEventListener("afterprint", onAfterPrint);
    if (fallback !== undefined) window.clearTimeout(fallback);
    host.remove();
    if (activePrintCleanup === cleanup) {
      activePrintCleanup = null;
      document.body.removeAttribute(PRINT_ACTIVE_ATTR);
    }
  };
  activePrintCleanup = cleanup;
  try {
    const filled = await fill(host);
    if (filled === false) {
      cleanup();
      return false;
    }
    // Chromium adds page title/date headers whenever @page leaves room for them. A final zero-margin
    // rule suppresses that browser chrome for every format; document hosts provide their own cloned
    // page padding where needed.
    const pageGuard = document.createElement("style");
    pageGuard.setAttribute("data-vr-print-page-guard", "");
    pageGuard.textContent = "@page { margin: 0 !important; }";
    host.appendChild(pageGuard);
    // Let the mounted content actually paint before the native print pipeline captures the page —
    // otherwise a just-set image src (even a decoded one) or freshly-attached-to-DOM markup can be
    // snapshotted mid-layout and print blank.
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
    // The owned preview window exists only on Windows. Avoid serializing the complete image host
    // on WKWebView/Linux, where the command is a guaranteed no-op; native print captures the live
    // host directly there. Windows still receives the full host so preview never drops pages.
    const previewOpened = isWindows()
      ? await openPrintPreviewWindow({
          html: host.innerHTML,
          shadowHtml: host.shadowRoot?.innerHTML ?? null,
        })
      : false;
    if (previewOpened) {
      cleanup();
      return true;
    }
    const printed = await printCurrentWebview();
    // Native print UI returns before the platform has captured this print-only host. Tearing it down
    // here races WKWebView's sheet and WebView2's system dialog, producing blank or wrong content.
    // Defer cleanup to `afterprint`, with a generous fallback for Windows runtimes that omit it.
    onAfterPrint = cleanup;
    fallback = window.setTimeout(cleanup, 90_000);
    window.addEventListener("afterprint", onAfterPrint, { once: true });
    return printed;
  } catch (e) {
    cleanup();
    throw e;
  }
}

/** Append a flat `<img>` print page and wait for it to decode, falling back to onload/onerror for
 *  runtimes without a working HTMLImageElement.decode. Flat images are used instead of canvases
 *  because WKWebView's print pass re-renders into a print context that canvas backing stores do
 *  not reliably survive. */
export async function mountPrintImage(
  host: HTMLDivElement,
  src: string,
  cssText: string,
): Promise<void> {
  const img = document.createElement("img");
  img.style.cssText = cssText;
  host.appendChild(img);
  await waitForPrintImage(img, src);
}
