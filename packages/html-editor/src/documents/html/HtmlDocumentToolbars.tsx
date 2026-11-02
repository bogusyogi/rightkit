import type { MutableRefObject, ReactNode } from "react";
import { DocBar } from "@rightkit/document-core/app/shell/docBar";
import { PreviewChrome } from "@rightkit/document-core/documents/PreviewChrome";
import type { Viewport } from "@rightkit/document-core/documents/previewViewport";
import { Sep } from "@rightkit/document-core/documents/toolbarPrimitives";
import type { BlockDraftSnapshot } from "@rightkit/document-core/app/blockDrafts";
import type { RichBlockOp, StructuralAction, StructuralSurface } from "./htmlOperationPlanner.js";
import type { HtmlEditorToolbarState, HtmlSafeBlockEditorHandle } from "./HtmlSafeBlockEditor.js";
import { HtmlBlockEditToolbar } from "./HtmlBlockEditToolbar.js";
import { HtmlRichEditToolbar } from "./HtmlRichEditToolbar.js";
import { HtmlSubpageStaleMarker } from "./HtmlSubpageStaleMarker.js";

interface AddressRowProps {
  address: string;
  dirty: boolean;
  viewport: Viewport;
  canGoBack: boolean;
  canGoForward: boolean;
  onBack: () => void;
  onForward: () => void;
  onViewport: (next: Viewport) => void;
  onReload: () => void;
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  trailing?: { label: string; ariaLabel: string; active: boolean; onClick: () => void };
}

/** The address capsule docked in the document bar's centre. */
export function HtmlAddressRow(props: AddressRowProps & { subpageStale?: boolean }) {
  const { subpageStale, ...chrome } = props;
  return (
    <DocBar>
      <div className="vr-capsule h-[42px] min-w-0 px-1">
        {subpageStale && <HtmlSubpageStaleMarker />}
        <PreviewChrome variant="capsule" {...chrome} />
      </div>
    </DocBar>
  );
}

interface RichEditRowProps {
  handleRef: MutableRefObject<HtmlSafeBlockEditorHandle | null>;
  snapshot: BlockDraftSnapshot;
  anchorHref?: string;
  anchorSrc?: string;
  formatting?: HtmlEditorToolbarState;
  onRichOp: (op: RichBlockOp) => void;
  onStructuralOp: (surface: StructuralSurface, action: StructuralAction) => void;
  canRichOp: (op: RichBlockOp) => boolean;
  canStructuralOp: (surface: StructuralSurface, action: StructuralAction) => boolean;
  onEditSource: () => void | Promise<void>;
}

/** Block-editing capsule that replaces the address row while a block is open for rich edit. */
export function HtmlRichEditRow(props: RichEditRowProps) {
  return (
    <div
      className="no-print vr-chrome vr-document-toolbar vr-document-toolbar-search-layout vr-html-authoring-row relative z-30 shrink-0 text-xs"
      data-testid="html-rich-edit-row"
    >
      {/* Single capsule docked in its own row under the preview chrome, like the other
          formats. The contextual block bar (transform/move/insert/delete) and the inline
          format bar sit side by side in the one pill, each scrolling independently inside it
          instead of stacking two docked rows. */}
      <DocBar>
        <div className="vr-capsule h-[42px] px-1">
          <div className="vr-document-toolbar-main min-w-0">
            <HtmlBlockEditToolbar
              handleRef={props.handleRef}
              snapshot={props.snapshot}
              onRichOp={props.onRichOp}
              onStructuralOp={props.onStructuralOp}
              canRichOp={props.canRichOp}
              canStructuralOp={props.canStructuralOp}
              onEditSource={props.onEditSource}
            />
          </div>
          <Sep />
          <div className="vr-document-toolbar-main min-w-0">
            <HtmlRichEditToolbar
              handleRef={props.handleRef}
              anchorHref={props.anchorHref}
              anchorSrc={props.anchorSrc}
              onRichOp={props.onRichOp}
              canRichOp={props.canRichOp}
              formatting={props.formatting}
            />
          </div>
        </div>
      </DocBar>
    </div>
  );
}

/** Browser-style status strip: the URL of the link currently under the pointer in the preview. */
export function HtmlLinkStatus({ href }: { href: string }) {
  return (
    <div
      data-vr-link-status
      className="no-print pointer-events-none absolute bottom-0 left-0 z-50 max-w-[70%] truncate rounded-tr border-r border-t border-line bg-panel-2 px-2 py-0.5 text-[11px] text-muted shadow-lg"
    >
      {href}
    </div>
  );
}

export function HtmlSiteRootNotice({
  pickingRoot,
  onChoose,
}: {
  pickingRoot: boolean;
  onChoose: () => void;
}) {
  return (
    <div
      data-vr-site-root-notice
      role="status"
      className="no-print absolute right-3 top-2 z-50 flex max-w-[80%] items-center gap-2 rounded border border-line bg-panel-2 px-2 py-1 text-[12px] text-muted shadow-lg"
    >
      <span>Site root not detected — root-relative links may not load.</span>
      <button
        type="button"
        data-vr-choose-site-root
        disabled={pickingRoot}
        className="shrink-0 text-accent underline disabled:opacity-60"
        onClick={onChoose}
      >
        Choose site root…
      </button>
    </div>
  );
}

