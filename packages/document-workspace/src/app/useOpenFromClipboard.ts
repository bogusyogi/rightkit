import { useCallback } from "react";
import { toast } from "@rightkit/platform-ui/headless";
import { normalizeLivePreviewUrl } from "@rightkit/document-core/documents/web/webUrl";
import { addRecentUrl } from "@rightkit/document-core/state/recentUrls";
import { readClipboardContent, openLinkedDocument } from "@rightkit/document-core/state/tauriBridge";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { localPathFromClipboardText } from "./clipboardPaths.js";

/** Open from clipboard: copied files (CF_HDROP) open like a drag-drop; nothing → a toast. */
export function useOpenFromClipboard({
  open,
  openLiveUrl,
  duplicateAsDraft,
  setError,
}: {
  open: (path: string) => unknown;
  openLiveUrl: (url: string) => unknown;
  duplicateAsDraft: (text: string, title?: string) => unknown;
  setError: (message: string | null) => void;
}) {
  return useCallback(async () => {
    const clip = await readClipboardContent();
    switch (clip.kind) {
      case "files":
        for (const p of clip.paths) void open(p);
        return;
      case "image":
        void open(clip.path);
        return;
      case "text":
        if (normalizeLivePreviewUrl(clip.text)) {
          addRecentUrl(clip.text.trim());
          void openLiveUrl(clip.text);
          return;
        }
        {
          const localPath = localPathFromClipboardText(clip.text);
          if (localPath) {
            const granted = await openLinkedDocument(localPath).catch((e) => {
              setError(errorMessage(e));
              return null;
            });
            if (granted) void open(granted);
            return;
          }
        }
        duplicateAsDraft(clip.text, "Pasted from clipboard");
        return;
      default:
        toast("Nothing on the clipboard to open", "info");
    }
  }, [open, openLiveUrl, duplicateAsDraft, setError]);
}
