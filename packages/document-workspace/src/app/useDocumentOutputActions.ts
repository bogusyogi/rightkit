import { useCallback } from "react";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { editImageExternal, printCurrentWebview } from "@rightkit/document-core/state/tauriBridge";
import { printPdf } from "@rightkit/document-core/app/documentPrinting";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { toast } from "@rightkit/platform-ui/headless";
import { logError, logEvent } from "@rightkit/document-core/state/diagnostics";

/** External-edit and print entry points (no shared document state beyond the error setter). */
export function useDocumentOutputActions(setError: (message: string | null) => void) {
  const onEditImageExternal = useCallback(
    (doc: LoadedDocument | null) => {
      if (doc?.format !== "image") return;
      void editImageExternal(doc.path).catch((e) => setError(errorMessage(e)));
    },
    [setError],
  );

  const onPrint = useCallback((doc: LoadedDocument | null) => {
    if (doc?.format === "web") {
      toast("Printing web tabs isn't supported yet", "info");
      return;
    }
    if (doc?.format === "pdf" && doc.assetUrl) {
      void printPdf(doc.assetUrl)
        .then((printed) => {
          if (printed) return;
          logError("print:failed:pdf");
          toast("Couldn't open print dialog", "error");
        })
        .catch(() => {
          logError("print:failed:pdf");
          toast("Couldn't open print dialog", "error");
        });
      return;
    }
    if (!window.dispatchEvent(new Event("viewright:print", { cancelable: true }))) return;
    // Frame previews claim print above; native documents use the window-level print command.
    logEvent("print:requested");
    void printCurrentWebview().then((printed) => {
      if (printed) return;
      logError("print:failed");
      toast("Couldn't open print dialog", "error");
    });
  }, []);

  return { onEditImageExternal, onPrint };
}
