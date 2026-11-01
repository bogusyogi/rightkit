import type { ComponentProps, Dispatch, ReactNode, RefObject, SetStateAction } from "react";
import type { ViewportMode } from "@rightkit/html-editor/documents/html/htmlDocumentShared";
import type { HtmlSafeBlockTarget } from "@rightkit/html-editor/documents/html/sourceMap";
import type { InspectMenuState } from "@rightkit/html-editor/documents/html/useHtmlInspect";
import type { HtmlSafeBlockEditorHandle, HtmlEditorToolbarState } from "@rightkit/html-editor/documents/html/HtmlSafeBlockEditor";
import { HtmlDocumentView } from "@rightkit/html-editor/documents/html/HtmlDocumentView";
import { HtmlDocumentSource } from "./HtmlDocumentSource.js";
import { HtmlDocumentEdit } from "@rightkit/html-editor/documents/html/HtmlDocumentEdit";
import {
  HtmlAddressRow,
  HtmlLinkStatus,
  HtmlRichEditRow,
  HtmlSiteRootNotice,
} from "@rightkit/html-editor/documents/html/HtmlDocumentToolbars";
import { HtmlSafeBlockEditor } from "@rightkit/html-editor/documents/html/HtmlSafeBlockEditor";
import { BlockedLinkPrompt } from "@rightkit/html-editor/documents/html/HtmlDocumentChrome";
import type { useHtmlNavigation } from "@rightkit/html-editor/documents/html/useHtmlNavigation";

type Blocked = ReturnType<typeof useHtmlNavigation>["blocked"];
type RichEditRef = RefObject<HtmlSafeBlockEditorHandle | null>;
type ViewportChoice = ComponentProps<typeof HtmlAddressRow>["viewport"];

export interface HtmlDocumentLayoutProps {
  /** Mode and visibility flags. */
  flags: {
    readOnly: boolean;
    active: boolean;
    drawer: boolean;
    sourceFullscreen: boolean;
    mode: "interactive" | "preview";
  };
  /** Top address capsule (or the editing capsule row when a block is open). */
  address: {
    activePath: string;
    activeDirty: boolean;
    subpageStale?: boolean;
    viewport: ViewportChoice;
    canGoBack: boolean;
    canGoForward: boolean;
    goHistory: (delta: number) => void;
    setViewportMode: Dispatch<SetStateAction<ViewportMode>>;
    onReload: () => void;
    findBar: ReactNode;
    findMode?: "find" | "replace";
    onRequestFind?: () => void;
    onRequestReplace?: () => void;
    onToggleSource: () => void;
  };
  /** Rich-edit session: authoring row, inline overlay, and their operations. */
  rich: {
    safeTarget: HtmlSafeBlockTarget | null;
    blockSnapshot: ComponentProps<typeof HtmlRichEditRow>["snapshot"] | undefined;
    blockIdentity: ComponentProps<typeof HtmlSafeBlockEditor>["identity"];
    hasChangeView: boolean;
    richEditRef: RichEditRef;
    richFormatting: HtmlEditorToolbarState | undefined;
    setRichFormatting: ComponentProps<typeof HtmlSafeBlockEditor>["onToolbarStateChange"];
    runBlockRichOp: ComponentProps<typeof HtmlRichEditRow>["onRichOp"];
    runBlockStructuralOp: ComponentProps<typeof HtmlRichEditRow>["onStructuralOp"];
    canRunRichOp: ComponentProps<typeof HtmlRichEditRow>["canRichOp"];
    canRunStructuralOp: ComponentProps<typeof HtmlRichEditRow>["canStructuralOp"];
    propertyAttributes: ComponentProps<typeof HtmlSafeBlockEditor>["propertyAttributes"];
    editRichSource: () => void;
    closeInlineEditor: () => void;
    inframeTarget: ComponentProps<typeof HtmlSafeBlockEditor>["iframeTarget"];
    setInframeTarget: ComponentProps<typeof HtmlSafeBlockEditor>["onIframeTargetChange"];
    iframeGeneration: ComponentProps<typeof HtmlSafeBlockEditor>["iframeGeneration"];
  };
  /** Preview surface (interactive iframe / Safe iframe) and its split container. */
  preview: Omit<
    ComponentProps<typeof HtmlDocumentView>,
    "mode" | "drawerOpen" | "overlay" | "active"
  > & {
    splitRef: RefObject<HTMLDivElement | null>;
    hoveredLink: string | null;
    pickingRoot: boolean;
    onChooseSiteRoot: () => void;
    showSiteRootNotice: boolean;
  };
  /** Source drawer (CodeMirror). */
  source: Omit<ComponentProps<typeof HtmlDocumentSource>, "active" | "sourceFullscreen"> & {
    onToggleFullscreen: () => void;
  };
  /** Element context menu and blocked-link prompt. */
  overlays: {
    inspectMenu: InspectMenuState | null;
    setInspectMenu: (menu: InspectMenuState | null) => void;
    handleHref: (href: string, background: boolean) => Promise<void> | void;
    copyMenuValue: (value: string) => void;
    inspectFromMenu: () => void;
    blocked: Blocked;
    setBlocked: (value: Blocked) => void;
    locateBlocked: () => Promise<void> | void;
    copyBlocked: () => Promise<void> | void;
  };
}

/** Pure render tree for the HTML document: toolbars, preview split, source drawer, and overlays. */
export function HtmlDocumentLayout({ flags, address, rich, preview, source, overlays }: HtmlDocumentLayoutProps) {
  const { readOnly, active, drawer, sourceFullscreen, mode } = flags;
  const {
    safeTarget,
    blockSnapshot,
    blockIdentity,
    hasChangeView,
    richEditRef,
    richFormatting,
    setRichFormatting,
    runBlockRichOp,
    runBlockStructuralOp,
    canRunRichOp,
    canRunStructuralOp,
    propertyAttributes,
    editRichSource,
    closeInlineEditor,
    inframeTarget,
    setInframeTarget,
    iframeGeneration,
  } = rich;
  const { splitRef, hoveredLink, pickingRoot, onChooseSiteRoot, showSiteRootNotice, ...viewProps } = preview;
  const overlay =
    !readOnly && safeTarget && blockSnapshot && (inframeTarget || !hasChangeView) ? (
      <HtmlSafeBlockEditor
        ref={richEditRef}
        identity={blockIdentity}
        snapshot={blockSnapshot}
        visualStyle={safeTarget.style}
        onClose={closeInlineEditor}
        onRichOp={(op) => void runBlockRichOp(op)}
        onToolbarStateChange={setRichFormatting}
        propertyTargetTag={safeTarget.src !== undefined ? "img" : undefined}
        propertyAttributes={propertyAttributes}
        onEditSource={editRichSource}
        iframeTarget={inframeTarget}
        mountMode={inframeTarget ? "inframe" : undefined}
        iframeGeneration={iframeGeneration}
        onIframeTargetChange={setInframeTarget}
      />
    ) : null;

  return (
    <div className="relative flex h-full min-h-0 flex-col bg-ink">
      {/* One docked row: the address capsule sits in the document bar's centre; while a block is
          being edited the editing capsule takes its place (never two stacked rows). */}
      {!(!readOnly && safeTarget && blockSnapshot) && (
        <HtmlAddressRow
          address={address.activePath}
          dirty={address.activeDirty}
          subpageStale={address.subpageStale}
          viewport={address.viewport}
          canGoBack={address.canGoBack}
          canGoForward={address.canGoForward}
          onBack={() => address.goHistory(-1)}
          onForward={() => address.goHistory(1)}
          onViewport={(next) => address.setViewportMode(next === "desktop" ? "auto" : next)}
          onReload={address.onReload}
          findBar={drawer ? undefined : address.findBar}
          findMode={drawer ? undefined : address.findMode}
          onRequestFind={address.onRequestFind}
          onRequestReplace={drawer ? undefined : address.onRequestReplace}
          trailing={
            readOnly
              ? undefined
              : {
                  label: "source",
                  ariaLabel: "Toggle source",
                  active: drawer,
                  onClick: address.onToggleSource,
                }
          }
        />
      )}

      {!readOnly && safeTarget && blockSnapshot && (
        <HtmlRichEditRow
          handleRef={richEditRef}
          snapshot={blockSnapshot}
          anchorHref={safeTarget.href}
          anchorSrc={safeTarget.src}
          formatting={richFormatting}
          onRichOp={(op) => void runBlockRichOp(op)}
          onStructuralOp={(surface, action) => void runBlockStructuralOp(surface, action)}
          canRichOp={canRunRichOp}
          canStructuralOp={canRunStructuralOp}
          onEditSource={editRichSource}
        />
      )}

      <div ref={splitRef} data-html-preview-split className="relative flex min-h-0 min-w-0 flex-1">
        {hoveredLink && <HtmlLinkStatus href={hoveredLink} />}
        {!sourceFullscreen && showSiteRootNotice && (
          <HtmlSiteRootNotice pickingRoot={pickingRoot} onChoose={onChooseSiteRoot} />
        )}
        {!sourceFullscreen && (
          <HtmlDocumentView
            {...viewProps}
            mode={mode}
            drawerOpen={drawer}
            active={active}
            overlay={overlay}
          />
        )}

        {drawer && (
          <HtmlDocumentSource
            {...source}
            active={active}
            sourceFullscreen={sourceFullscreen}
          />
        )}
      </div>

      {overlays.inspectMenu && (
        <HtmlDocumentEdit
          inspectMenu={overlays.inspectMenu}
          onClose={() => overlays.setInspectMenu(null)}
          onCopyValue={overlays.copyMenuValue}
          onOpenLinkNewTab={(href) => {
            overlays.setInspectMenu(null);
            void overlays.handleHref(href, true);
          }}
          onViewSource={overlays.inspectFromMenu}
        />
      )}

      {overlays.blocked && (
        <BlockedLinkPrompt
          href={overlays.blocked.href}
          reason={overlays.blocked.reason}
          onLocate={() => void overlays.locateBlocked()}
          onCopy={() => void overlays.copyBlocked()}
          onDismiss={() => overlays.setBlocked(null)}
        />
      )}
    </div>
  );
}
