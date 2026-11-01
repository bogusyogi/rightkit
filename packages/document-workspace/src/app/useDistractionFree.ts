import { useRef, useState } from "react";
import { toast, useDismiss } from "@rightkit/platform-ui/headless";
import { shortcutLabel } from "@rightkit/document-core/platform";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { createBlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { resolveDocumentDrafts } from "./draftResolution.js";

type Registry = ReturnType<typeof createBlockDraftRegistry>;
type PendingFullscreenDraft = { drafts: ReturnType<Registry["snapshotModified"]> };

/** Distraction-free reading mode: the toggle (guarded by unresolved block drafts), the Esc-exit
 * effect, and the confirm-dialog handlers. Extracted verbatim from App.tsx. */
export function useDistractionFree({
  active,
  blockDrafts,
  overlayOpen,
}: {
  active: LoadedDocument | null;
  blockDrafts: Registry;
  /** True while settings / palette / help / open-url own Esc. */
  overlayOpen: boolean;
}) {
  const [distractionFree, setDistractionFree] = useState(false);
  const [pendingFullscreenDraft, setPendingFullscreenDraft] =
    useState<PendingFullscreenDraft | null>(null);

  const flipDistractionFree = () =>
    setDistractionFree((on) => {
      if (!on)
        toast(
          `Distraction-free — press ${shortcutLabel("esc")} or ${shortcutLabel("mod+shift+f")} to exit`,
          "info",
        );
      return !on;
    });

  const toggleDistractionFree = () => {
    const path = active?.path;
    if (path && blockDrafts.snapshotModified(path).length > 0) {
      setPendingFullscreenDraft({ drafts: blockDrafts.snapshotModified(path) });
      return;
    }
    // Clean draft closes silently before entering fullscreen
    if (path) {
      const clean = blockDrafts.snapshotAll(path);
      for (const d of clean) blockDrafts.discard(d.identity);
    }
    flipDistractionFree();
  };

  // Esc leaves distraction-free reading (when nothing else owns Esc).
  // Bubble phase on the window, like the handler it replaces, so an overlay or modal that takes
  // Escape in the capture phase keeps its own Escape.
  const windowTarget = useRef<EventTarget | null>(typeof window === "undefined" ? null : window);
  useDismiss(() => setDistractionFree(false), {
    enabled: distractionFree && !overlayOpen,
    escapeTarget: windowTarget,
    outside: false,
    preventDefault: true,
    stopPropagation: false,
    imeGuard: false,
  });

  const onDone = async () => {
    const path = active?.path;
    if (!path) {
      setPendingFullscreenDraft(null);
      return;
    }
    const outcome = await resolveDocumentDrafts(blockDrafts, path, "view-change", "done");
    if (outcome !== "resolved") {
      setPendingFullscreenDraft({ drafts: blockDrafts.snapshotModified(path) });
      return;
    }
    setPendingFullscreenDraft(null);
    flipDistractionFree();
  };
  const onDiscard = async () => {
    const path = active?.path;
    if (path) await resolveDocumentDrafts(blockDrafts, path, "view-change", "discard");
    setPendingFullscreenDraft(null);
    flipDistractionFree();
  };
  const confirm = pendingFullscreenDraft
    ? {
        onDone,
        onDiscard,
        onStay: () => setPendingFullscreenDraft(null),
        error:
          pendingFullscreenDraft.drafts.find((d) => d.error)?.error ??
          pendingFullscreenDraft.drafts.find((d) => d.reason)?.reason ??
          null,
      }
    : null;
  return { distractionFree, toggleDistractionFree, fullscreenConfirm: confirm };
}
