import { ModalShell } from "@rightkit/react-shell/react";
import { memo } from "react";

import Icon from "@rightkit/agent-ui-core/components/Icon";

// Image zoom viewer used by `ImageGallery` and `ReferenceChip` to render a single reference image
// at full size. Dialog semantics, Escape/backdrop dismissal, focus trap and focus restore are
// RightKit's `ModalShell` (@rightkit/react-shell); this adapter only supplies the image content.
// Controlled: `open` toggles, `onClose` clears.

export interface ImageZoomViewerProps {
  open: boolean;
  src: string;
  alt: string;
  caption?: string;
  onClose: () => void;
}

function ImageZoomViewerImpl({ open, src, alt, caption, onClose }: ImageZoomViewerProps) {
  if (!open) return null;

  return (
    <ModalShell
      label={alt}
      onClose={onClose}
      width={Math.min(1200, typeof window === "undefined" ? 1200 : window.innerWidth * 0.9)}
      className="!bg-transparent !border-0 !shadow-none !p-0"
      overlayClassName="bg-bg/85 backdrop-blur-md"
    >
      <div data-image-zoom-viewer="true" className="relative max-h-[90vh] max-w-[90vw]">
        <img
          src={src}
          alt={alt}
          className="max-h-[90vh] max-w-[90vw] rounded-xl border border-line object-contain shadow-app"
        />
        {caption ? (
          <div className="mt-2 text-center font-mono text-[12px] text-text-2">{caption}</div>
        ) : null}
        <button
          type="button"
          aria-label="Close image viewer"
          onClick={onClose}
          className="absolute -top-3 -right-3 grid h-8 w-8 place-items-center rounded-full border border-line bg-panel text-text-2 hover:text-text"
        >
          <Icon name="close" size={14} />
        </button>
      </div>
    </ModalShell>
  );
}

export const ImageZoomViewer = memo(ImageZoomViewerImpl);
export default ImageZoomViewer;
