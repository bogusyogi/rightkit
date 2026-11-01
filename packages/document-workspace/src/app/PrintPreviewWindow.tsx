import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { printCurrentWebview } from "@rightkit/document-core/state/tauriBridge";
import { takePrintPreviewPayload } from "@rightkit/document-core/app/nativePrintPreview";

export function isPrintPreviewWindow(): boolean {
  return new URLSearchParams(window.location.search).has("viewright-print-preview");
}

function PrintPreviewWindow() {
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    const currentWindow = getCurrentWindow();
    const close = () => void currentWindow.close();
    window.addEventListener("afterprint", close, { once: true });

    void (async () => {
      try {
        const payload = await takePrintPreviewPayload();
        if (disposed) return;

        const host = document.createElement("div");
        host.dataset.vrPrintHost = "";
        host.innerHTML = payload.html;
        if (payload.shadowHtml !== null) {
          host.attachShadow({ mode: "open" }).innerHTML = payload.shadowHtml;
        }
        document.body.appendChild(host);
        document.body.setAttribute("data-vr-print-active", "");

        await currentWindow.show();
        await currentWindow.setFocus();
        await new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        );
        await printCurrentWebview();
      } catch (cause) {
        if (!disposed) setError(cause instanceof Error ? cause.message : String(cause));
        await currentWindow.show();
      }
    })();

    return () => {
      disposed = true;
      window.removeEventListener("afterprint", close);
    };
  }, []);

  return (
    <main
      style={{
        minHeight: "100vh",
        display: "grid",
        placeItems: "center",
        background: "#11110f",
        color: "#f2efe8",
        font: "600 15px/1.5 sans-serif",
      }}
    >
      {error ? `Print preview failed: ${error}` : "Preparing print preview..."}
    </main>
  );
}

export function mountPrintPreviewWindow(root: HTMLElement): void {
  createRoot(root).render(
    <React.StrictMode>
      <PrintPreviewWindow />
    </React.StrictMode>,
  );
}
