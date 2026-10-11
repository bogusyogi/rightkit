import {
  lazy,
  Suspense,
  type RefObject,
  type SyntheticEvent,
  type PointerEvent as ReactPointerEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { resolveIcon } from "../icons.js";
import type { MediaAdapters } from "../adapters.js";
import type { CropRect, ImageEdit, TextLayer } from "./imageEdit.js";
import type { ImageSize, ImageTransform } from "./imageViewerState.js";
import type { ImageTool } from "./ImageViewerToolbar.js";

const PAN_KEY_STEP = 44;

// Both tools are only ever mounted once the user picks that tool, so load their (heavy) modules
// on demand: fabric for the text overlay, react-image-crop + its CSS for the crop tool.
const FabricTextOverlay = lazy(() =>
  import("./FabricTextOverlay.js").then((m) => ({ default: m.FabricTextOverlay })),
);
const ImageCropTool = lazy(() =>
  import("./ImageCropTool.js").then((m) => ({ default: m.ImageCropTool })),
);

function isTextEntry(target: EventTarget): boolean {
  const element = target as HTMLElement;
  const tag = element.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || element.isContentEditable === true;
}

export function ImageViewerStage({
  displayName,
  displaySrc,
  frameRef,
  siblingIndex,
  siblingTotal,
  canPrev,
  canNext,
  onPageSibling,
  dragging,
  panning,
  view,
  stageSize,
  stageTransform,
  imageSize,
  tool,
  edit,
  cropDraft,
  cropRatio,
  filter,
  selectedText,
  adapters,
  onWheel,
  onPointerDown,
  onPointerMove,
  onPointerStop,
  onDoubleClick,
  onKeyPan,
  onZoomIn,
  onZoomOut,
  onActual,
  onFit,
  onImageLoad,
  onCropChange,
  onTextSelect,
  onTextChange,
}: {
  displayName: string;
  displaySrc: string;
  frameRef: RefObject<HTMLDivElement | null>;
  /** Folder paging: 0-based position of this image among same-folder siblings (-1 if unknown). */
  siblingIndex: number;
  siblingTotal: number;
  canPrev: boolean;
  canNext: boolean;
  /** Page by a signed step: -1 = previous image, +1 = next image. */
  onPageSibling: (delta: number) => void;
  dragging: boolean;
  panning: boolean;
  view: ImageTransform;
  stageSize: ImageSize | null;
  stageTransform: string;
  imageSize: ImageSize | null;
  tool: ImageTool;
  edit: ImageEdit;
  cropDraft: CropRect | null;
  cropRatio: number | null;
  filter: string;
  selectedText: string | null;
  adapters: MediaAdapters;
  onWheel: (event: ReactWheelEvent<HTMLDivElement>) => void;
  onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerStop: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onDoubleClick: () => void;
  onKeyPan: (dx: number, dy: number) => void;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onActual: () => void;
  onFit: () => void;
  onImageLoad: (event: SyntheticEvent<HTMLImageElement>) => void;
  onCropChange: (crop: CropRect) => void;
  onTextSelect: (id: string | null) => void;
  onTextChange: (id: string, patch: Partial<TextLayer>) => void;
}) {
  return (
    <div
      ref={frameRef}
      data-rk-image-stage
      role="group"
      aria-roledescription="image viewer"
      aria-label={displayName}
      tabIndex={0}
      className="rk-iv-stage"
      style={{ cursor: dragging ? "grabbing" : panning ? "grab" : "default", touchAction: "none" }}
      onWheel={onWheel}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerStop}
      onPointerCancel={onPointerStop}
      onDoubleClick={onDoubleClick}
      onKeyDown={(event) => {
        // Leave browser shortcuts (Ctrl/Cmd +/-/0) and text entry (the text tool's editor) alone.
        if (event.ctrlKey || event.metaKey || event.altKey || isTextEntry(event.target)) return;
        if (event.key === "+" || event.key === "=") onZoomIn();
        else if (event.key === "-" || event.key === "_") onZoomOut();
        else if (event.key === "0") onActual();
        else if (event.key.toLowerCase() === "f") onFit();
        // Arrows pan only when the image can pan; at fit they stay unhandled so folder paging receives them.
        else if (event.key.startsWith("Arrow") && !panning) return;
        else if (event.key === "ArrowLeft") onKeyPan(PAN_KEY_STEP, 0);
        else if (event.key === "ArrowRight") onKeyPan(-PAN_KEY_STEP, 0);
        else if (event.key === "ArrowUp") onKeyPan(0, PAN_KEY_STEP);
        else if (event.key === "ArrowDown") onKeyPan(0, -PAN_KEY_STEP);
        else return;
        event.preventDefault();
      }}
    >
      <div
        className="rk-iv-canvas-pos"
        style={{
          transform: `translate(-50%, -50%) translate(${view.offsetX}px, ${view.offsetY}px)`,
        }}
      >
        <div
          className="rk-iv-canvas-box"
          style={{
            width: stageSize?.width,
            height: stageSize?.height,
            transform: stageTransform,
          }}
        >
          <div
            className="rk-iv-canvas-inner"
            style={{
              width: imageSize?.width,
              height: imageSize?.height,
              transform:
                tool !== "crop" && edit.crop
                  ? `translate(${-edit.crop.x}px, ${-edit.crop.y}px)`
                  : undefined,
            }}
          >
            {tool === "crop" && imageSize && cropDraft ? (
              <Suspense fallback={null}>
                <ImageCropTool
                  src={displaySrc}
                  imageSize={imageSize}
                  crop={cropDraft}
                  aspect={cropRatio}
                  filter={filter}
                  onImageLoad={onImageLoad}
                  onChange={onCropChange}
                />
              </Suspense>
            ) : (
              <img
                data-rk-image-printable
                src={displaySrc}
                alt=""
                className="rk-iv-img"
                style={{ filter, width: imageSize?.width, height: imageSize?.height }}
                draggable={false}
                onLoad={onImageLoad}
              />
            )}
            {tool !== "crop" && imageSize && edit.text.length > 0 && (
              <Suspense fallback={null}>
                <FabricTextOverlay
                  text={edit.text}
                  selectedId={selectedText}
                  imageSize={imageSize}
                  interactive={tool === "text"}
                  adapters={adapters}
                  onSelect={onTextSelect}
                  onChange={onTextChange}
                />
              </Suspense>
            )}
          </div>
        </div>
      </div>
      {stageSize && (
        <div className="rk-iv-badge rk-iv-badge-left">
          {stageSize.width} × {stageSize.height}
        </div>
      )}
      {siblingTotal > 1 && (
        <>
          <SiblingButton side="left" disabled={!canPrev} onClick={() => onPageSibling(-1)} icons={adapters.icons} />
          <SiblingButton side="right" disabled={!canNext} onClick={() => onPageSibling(1)} icons={adapters.icons} />
          {siblingIndex >= 0 && (
            <div className="rk-iv-badge rk-iv-badge-center">
              {siblingIndex + 1} / {siblingTotal}
            </div>
          )}
        </>
      )}
    </div>
  );
}

// Hover-revealed prev/next pager. Disabled (and dimmed) at the folder ends.
function SiblingButton({
  side,
  disabled,
  onClick,
  icons,
}: {
  side: "left" | "right";
  disabled: boolean;
  onClick: () => void;
  icons?: MediaAdapters["icons"];
}) {
  const isLeft = side === "left";
  return (
    <button
      type="button"
      aria-label={isLeft ? "Previous image" : "Next image"}
      disabled={disabled}
      onClick={onClick}
      onPointerDown={(e) => e.stopPropagation()}
      className={`rk-iv-sibling ${isLeft ? "rk-iv-sibling-left" : "rk-iv-sibling-right"}`}
    >
      {resolveIcon(isLeft ? "caretLeft" : "caretRight", icons)}
    </button>
  );
}
