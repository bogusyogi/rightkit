// SearchProvider for the in-frame interactive-HTML find engine. Thin wrapper over usePreviewFind
// (shared with the MDX preview): supplies the interactive postMessage senders + the `html-preview` kind.
// Only active in interactive mode with the drawer CLOSED — when the drawer is open it owns find
// (searches the source); Safe mode registers its own provider.

import type { RefObject } from "react";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import { usePreviewFind } from "@rightkit/document-core/documents/usePreviewFind";
import {
  postInteractiveFindClear,
  postInteractiveFindQuery,
  postInteractiveFindStep,
} from "./htmlInteractiveProtocol.js";

const HTML_FIND_PROTOCOL = {
  clear: postInteractiveFindClear,
  query: postInteractiveFindQuery,
  step: postInteractiveFindStep,
};

interface HtmlPreviewFindDeps {
  iframeRef: RefObject<HTMLIFrameElement | null>;
  sessionId: string;
  mode: "interactive" | "preview";
  drawerOpen: boolean;
  error: string | null;
  registerSearchProvider: (provider: SearchProvider | null) => void;
}

export function useHtmlPreviewFind({
  iframeRef,
  sessionId,
  mode,
  drawerOpen,
  error,
  registerSearchProvider,
}: HtmlPreviewFindDeps) {
  return usePreviewFind({
    iframeRef,
    sessionId,
    enabled: mode === "interactive" && !drawerOpen && !error,
    kind: "html-preview",
    protocol: HTML_FIND_PROTOCOL,
    registerSearchProvider,
  });
}
