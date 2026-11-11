import { useEffect, useState } from "react";
import type { LoadedDocument, WebDocument } from "@rightkit/document-core/documents/types";
import { openDevtools } from "@rightkit/document-core/state/tauriBridge";
import { isHostAppOrigin } from "@rightkit/document-core/documents/web/webUrl";
import { PreviewMessage } from "@rightkit/document-core/documents/PreviewMessage";
import { viewportWidthPx, type Viewport } from "@rightkit/document-core/documents/previewViewport";
import { PreviewChrome } from "@rightkit/document-core/documents/PreviewChrome";
import { acquireResource } from "@rightkit/document-core/app/resourceCensus";
import { hostAppName } from "../../appName.js";
import { documentEvent } from "@rightkit/document-core/host";

interface Props {
  doc: LoadedDocument;
}

/**
 * Preview of an explicit URL (Open URL → http(s)://...). Local `.html` files are NOT
 * handled here anymore — they're owned by the normal html document tab (HtmlDocument), which renders
 * the interactive/code/safe modes. `web` tabs are session-only URL previews.
 */
export function LivePreviewPane({ doc }: Props) {
  if (!doc.web) {
    return (
      <PreviewMessage
        title="Preview unavailable"
        body="This tab is missing its web session metadata."
      />
    );
  }
  return <LiveUrlPreview web={doc.web} documentPath={doc.path} />;
}

function LiveUrlPreview({ web, documentPath }: { web: WebDocument; documentPath: string }) {
  const [reloadKey, setReloadKey] = useState(0);
  const [viewport, setViewport] = useState<Viewport>("desktop");
  const blockedOwnOrigin = isHostAppOrigin(web.url);
  useEffect(() => {
    if (blockedOwnOrigin) return;
    const release = acquireResource({
      kind: "web-iframe",
      documentPath,
      format: "web",
    });
    return release;
  }, [blockedOwnOrigin, documentPath]);

  // Ctrl+R / F5 (dispatched from the global shortcut handler as documentEvent("web-reload")) remounts the
  // iframe — same as the chrome reload button — so the URL re-fetches from its server.
  useEffect(() => {
    const onReload = () => setReloadKey((n) => n + 1);
    window.addEventListener(documentEvent("web-reload"), onReload);
    return () => window.removeEventListener(documentEvent("web-reload"), onReload);
  }, []);
  const width = viewportWidthPx(viewport); // null = full-width desktop; otherwise a device frame

  // Security note (audited 2026-06-02): `allow-scripts allow-same-origin` is the documented
  // sandbox-escape combo ONLY when the framed content is same-origin with this app — then it could
  // reach window.frameElement and strip its own sandbox. Here `src` is a CROSS-ORIGIN web URL, so the
  // framed page cannot access frameElement and cannot escape. `allow-same-origin` is required so the
  // previewed page keeps its own cookies/storage/fetch + HMR. The self-origin block prevents the app
  // from framing itself — do NOT remove allow-same-origin; no real security gain.
  // `allow-popups` lets target=_blank links fire a new-window request, which Rust
  // (context_menu::route_new_windows_to_tabs) intercepts for loopback URLs and reopens as a VR web
  // tab; external target=_blank links still go to the OS browser by design.
  const frame = (
    <iframe
      key={reloadKey}
      title="Live URL preview"
      src={web.url}
      sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
      className="h-full w-full border-0 bg-panel"
    />
  );

  return (
    <div className="flex h-full min-h-0 flex-col bg-ink">
      <PreviewChrome
        address={web.url}
        viewport={viewport}
        onViewport={setViewport}
        onReload={() => setReloadKey((n) => n + 1)}
        trailing={{
          label: "devtools",
          ariaLabel: "Open DevTools",
          onClick: () => void openDevtools(),
        }}
      />
      {blockedOwnOrigin ? (
        <PreviewMessage
          title={`${hostAppName()} cannot frame itself`}
          body="Open a different localhost app or port."
        />
      ) : width ? (
        // Browser-style device frame: constrain the preview width, centered on a backdrop.
        <div className="flex min-h-0 flex-1 justify-center overflow-auto bg-ink/50 p-4">
          <div
            className="h-full overflow-hidden rounded-md border border-line bg-panel shadow-2xl"
            style={{ width, maxWidth: "100%" }}
          >
            {frame}
          </div>
        </div>
      ) : (
        <div className="min-h-0 flex-1">{frame}</div>
      )}
    </div>
  );
}
