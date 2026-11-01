import { memo, useCallback, useState } from "react";

import Icon from "@rightkit/agent-ui-core/components/Icon";
import ImageZoomViewer from "./ImageZoomViewer.js";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

// Image-gallery view of image references. Shows up to GALLERY_VISIBLE_LIMIT
// thumbnails; opens the `ImageZoomViewer` on click. Useful for transcripts
// where multiple image references appear together.

const GALLERY_VISIBLE_LIMIT = 6;

export interface ImageGalleryProps {
  references: ReferenceObject[];
  onRemove?: (id: string) => void;
}

function ImageGalleryImpl({ references, onRemove }: ImageGalleryProps) {
  const [zoomIndex, setZoomIndex] = useState<number | null>(null);

  const images = references.filter(
    (ref) => ref.kind === "image" && (ref.previewUrl || ref.destination),
  );
  const openZoom = useCallback((index: number) => setZoomIndex(index), []);
  const closeZoom = useCallback(() => setZoomIndex(null), []);
  if (images.length === 0) return null;

  const visible = images.slice(0, GALLERY_VISIBLE_LIMIT);
  const overflow = images.length - visible.length;
  const zoomImage = zoomIndex !== null ? images[zoomIndex] : null;

  return (
    <fieldset data-image-gallery="true" className="flex flex-wrap gap-1.5">
      <legend className="sr-only">Image references</legend>
      {visible.map((ref, index) => (
        <div key={ref.id} className="group relative h-16 w-16">
          <button
            type="button"
            onClick={() => openZoom(index)}
            aria-label={`Open ${ref.filename ?? ref.destination}`}
            className="h-full w-full overflow-hidden rounded-lg border border-line bg-bg focus:outline focus:outline-2 focus:outline-accent"
          >
            {ref.previewUrl ? (
              <img
                src={ref.previewUrl}
                alt={ref.filename ?? ref.destination}
                className="h-full w-full object-cover"
                loading="lazy"
              />
            ) : (
              <span className="flex h-full w-full items-center justify-center bg-panel-2 text-text-3">
                <Icon name="image" size={18} />
              </span>
            )}
          </button>
          {onRemove ? (
            <button
              type="button"
              aria-label={`Remove ${ref.filename ?? ref.destination}`}
              onClick={(event) => {
                event.stopPropagation();
                onRemove(ref.id);
              }}
              className="absolute right-1 top-1 grid h-6 w-6 cursor-pointer place-items-center rounded-full border border-line bg-panel text-text-3 opacity-0 hover:text-text group-hover:opacity-100 group-focus-within:opacity-100"
            >
              <Icon name="close" size={10} />
            </button>
          ) : null}
        </div>
      ))}
      {overflow > 0 ? (
        <span
          role="status"
          aria-label={`${overflow} more image references`}
          title={`${overflow} more image references`}
          className="grid h-16 w-16 place-items-center rounded-lg border border-line bg-panel text-[12px] text-text-3"
        >
          +{overflow}
        </span>
      ) : null}
      {zoomImage ? (
        <ImageZoomViewer
          open
          src={zoomImage.previewUrl ?? zoomImage.destination}
          alt={zoomImage.filename ?? zoomImage.destination}
          caption={zoomImage.filename}
          onClose={closeZoom}
        />
      ) : null}
    </fieldset>
  );
}

export const ImageGallery = memo(ImageGalleryImpl);
export default ImageGallery;
