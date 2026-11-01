// SearchProvider for the in-frame MDX find engine. Thin wrapper over usePreviewFind (shared with the
// interactive HTML preview): supplies the MDX postMessage senders + the `markdown-preview` kind, and
// arms the provider only when the frame has content to find and no compile error.

import type { RefObject } from "react";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import { usePreviewFind } from "@rightkit/document-core/documents/usePreviewFind";
import { postMdxFindClear, postMdxFindQuery, postMdxFindStep } from "./mdxProtocol.js";

const MDX_FIND_PROTOCOL = {
  clear: postMdxFindClear,
  query: postMdxFindQuery,
  step: postMdxFindStep,
};

interface MdxPreviewFindDeps {
  iframeRef: RefObject<HTMLIFrameElement | null>;
  sessionId: string;
  /** Register the search provider only when the iframe has content to find and no compile error. */
  ready: boolean;
  registerSearchProvider: (provider: SearchProvider | null) => void;
}

export function useMdxPreviewFind({
  iframeRef,
  sessionId,
  ready,
  registerSearchProvider,
}: MdxPreviewFindDeps) {
  // mdx is structurally a markdown preview, so it reuses the FindBar's existing "markdown-preview" branch.
  return usePreviewFind({
    iframeRef,
    sessionId,
    enabled: ready,
    kind: "markdown-preview",
    protocol: MDX_FIND_PROTOCOL,
    registerSearchProvider,
  });
}
