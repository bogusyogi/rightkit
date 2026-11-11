// the host app host for @rightkit/media-ui's image viewer/editor. Reproduces the former local
// layout: tool panels in-flow above the stage inside the chrome toolbar, and only the capsule
// docked into App's single document bar (via DocBar, which renders nothing for inactive panes).
import { useState } from "react";
import { CaretLeft } from "@phosphor-icons/react/dist/icons/CaretLeft";
import { CaretRight } from "@phosphor-icons/react/dist/icons/CaretRight";
import type { ImageSaveFunction, MediaAdapters } from "@rightkit/media-ui";
import { ImageViewer as RightKitImageViewer } from "@rightkit/media-ui/image";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { DocBar } from "@rightkit/document-core/app/shell/docBar";
import { BUNDLED_FONTS } from "@rightkit/document-core/settings/fonts";
import { listSiblingImages, listSystemFonts } from "@rightkit/document-core/state/tauriBridge";
import { printImageDataUrl } from "@rightkit/document-core/app/documentPrinting";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { toast } from "@rightkit/platform-ui/headless";
import { acquireResource, updateResource, type ResourceKind } from "@rightkit/document-core/app/resourceCensus";
import { hostAppName } from "../appName.js";
import { documentEvent } from "@rightkit/document-core/host";

const ADAPTERS: MediaAdapters = {
  icons: {
    caretLeft: <CaretLeft size={18} weight="regular" aria-hidden="true" />,
    caretRight: <CaretRight size={18} weight="regular" aria-hidden="true" />,
  },
  listImageSiblings: listSiblingImages,
  listSystemFonts,
  bundledFonts: BUNDLED_FONTS,
  bundledFontsLabel: `${hostAppName()} fonts`,
  bundledFontsEmptyOption: hostAppName(),
  defaultFontId: "inter",
  fallbackFontStack: "'Inter', system-ui, sans-serif",
  printImage: printImageDataUrl,
  acquireResource: ({ kind, documentPath, estimatedBytes, dirty, stateExternalized }) => {
    const release = acquireResource({
      kind: kind as ResourceKind,
      documentPath,
      estimatedBytes,
      dirty,
      stateExternalized,
      format: "image",
    });
    return {
      release,
      update: (next) =>
        updateResource(release, {
          ...(next.dirty === undefined ? {} : { dirty: next.dirty }),
          ...(next.stateExternalized === undefined
            ? {}
            : { stateExternalized: next.stateExternalized }),
        }),
    };
  },
  notify: (message, severity) => toast(message, severity),
  // media-ui reports only the "print:failed" tag; any other tag has no allowlisted label.
  logError: (tag) => {
    if (tag === "print:failed") logError(tag);
  },
};

interface Props {
  doc: LoadedDocument;
  active?: boolean;
  onChange: () => void;
  registerSave: (fn: ImageSaveFunction | null) => void;
  /** Page to the previous/next image in the same folder, replacing this tab in place. */
  onOpenSibling?: (currentPath: string, targetPath: string) => void;
  /** Read mode: no edit tools — zoom/fit/paging stay, the stage is pure view. */
  readOnly?: boolean;
}

export function ImageViewer({ doc, ...props }: Props) {
  const [panelsHost, setPanelsHost] = useState<HTMLDivElement | null>(null);
  const [capsuleHost, setCapsuleHost] = useState<HTMLDivElement | null>(null);
  const displaySrc = doc.assetUrl || doc.content;
  return (
    <div data-vr-image-host="" className="flex h-full min-h-0 flex-col bg-ink">
      <div className="no-print vr-chrome vr-document-toolbar relative flex flex-shrink-0 flex-col text-chrome-xs text-muted">
        <div ref={setPanelsHost} className="contents" />
        <div className="relative z-30 shrink-0">
          <DocBar>
            <div ref={setCapsuleHost} className="vr-capsule vr-image-capsule h-[42px] px-1" />
          </DocBar>
        </div>
      </div>
      <RightKitImageViewer
        {...props}
        path={doc.path}
        displayName={doc.displayName}
        src={doc.content || displaySrc}
        displaySrc={displaySrc}
        printEventName={documentEvent("print")}
        toolbarContainer={capsuleHost}
        panelsContainer={panelsHost}
        adapters={ADAPTERS}
      />
    </div>
  );
}
