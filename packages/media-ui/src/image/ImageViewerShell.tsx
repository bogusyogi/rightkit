import { createPortal } from "react-dom";
import {
  useMemo,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
  type SyntheticEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import type { MediaAdapters } from "../adapters.js";
import type { ImageSize, ImageTransform } from "./imageViewerState.js";
import { ImageViewerStage } from "./ImageViewerStage.js";
import {
  ImageViewerCapsule,
  ImageViewerToolPanels,
  type ImageTool,
  type ImageViewerToolbarProps,
} from "./ImageViewerToolbar.js";
import { cssFilterForAdjustments, type CropRect, type ImageEdit, type TextLayer } from "./imageEdit.js";

type ToolbarHandlers = Pick<
  ImageViewerToolbarProps,
  | "onTool"
  | "onFit"
  | "onActual"
  | "onZoomIn"
  | "onZoomOut"
  | "onZoomPreset"
  | "onRotateLeft"
  | "onRotateRight"
  | "onFlipX"
  | "onFlipY"
  | "onReset"
  | "onUndo"
  | "onRedo"
  | "onCropRatio"
  | "onCropApply"
  | "onCropCancel"
  | "onCropClear"
  | "onAddText"
  | "onTextPatch"
  | "onTextDelete"
  | "onAdjustment"
  | "onResize"
>;

interface ImageViewerShellProps extends ToolbarHandlers {
  displayName: string;
  displaySrc: string;
  frameRef: RefObject<HTMLDivElement | null>;
  siblingIndex: number;
  siblingTotal: number;
  canPrev: boolean;
  canNext: boolean;
  dragging: boolean;
  panning: boolean;
  ready: boolean;
  edited: boolean;
  canUndoEdit: boolean;
  canRedoEdit: boolean;
  view: ImageTransform;
  stageSize: ImageSize | null;
  imageSize: ImageSize | null;
  tool: ImageTool;
  readOnly?: boolean;
  edit: ImageEdit;
  cropDraft: CropRect | null;
  cropRatio: number | null;
  selectedText: string | null;
  selectedLayer: TextLayer | null;
  adapters: MediaAdapters;
  toolbarContainer?: HTMLElement | null;
  panelsContainer?: HTMLElement | null;
  toolbarLabel?: string;
  onPageSibling: (delta: number) => void;
  onWheel: (event: ReactWheelEvent<HTMLDivElement>) => void;
  onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerStop: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onKeyPan: (dx: number, dy: number) => void;
  onImageLoad: (event: SyntheticEvent<HTMLImageElement>) => void;
  onCropChange: (crop: CropRect) => void;
  onTextSelect: (id: string | null) => void;
  onTextChange: (id: string, patch: Partial<TextLayer>) => void;
}

export function ImageViewerShell(props: ImageViewerShellProps) {
  const {
    displayName,
    displaySrc,
    frameRef,
    siblingIndex,
    siblingTotal,
    canPrev,
    canNext,
    dragging,
    panning,
    ready,
    edited,
    canUndoEdit,
    canRedoEdit,
    view,
    stageSize,
    imageSize,
    tool,
    readOnly = false,
    edit,
    cropDraft,
    cropRatio,
    selectedText,
    selectedLayer,
    adapters,
  } = props;
  const filter = cssFilterForAdjustments(edit.adjustments);
  const stageTransform = useMemo(
    () =>
      `rotate(${edit.rotation}deg) scale(${view.zoom}) scaleX(${edit.flipX ? -1 : 1}) scaleY(${edit.flipY ? -1 : 1})`,
    [edit.flipX, edit.flipY, edit.rotation, view.zoom],
  );

  const toolbarProps: ImageViewerToolbarProps = {
    toolbarLabel: props.toolbarLabel,
    tool,
    readOnly,
    zoomLabel: `${Math.round(view.zoom * 100)}%`,
    fitActive: view.mode === "fit",
    actualActive: view.mode === "actual" && view.zoom === 1,
    disabled: !ready,
    edited,
    canUndo: canUndoEdit,
    canRedo: canRedoEdit,
    flippedX: edit.flipX,
    flippedY: edit.flipY,
    adjustments: edit.adjustments,
    resize: edit.resize,
    cropRatio,
    selectedText: selectedLayer,
    adapters,
    onTool: props.onTool,
    onFit: props.onFit,
    onActual: props.onActual,
    onZoomIn: props.onZoomIn,
    onZoomOut: props.onZoomOut,
    onZoomPreset: props.onZoomPreset,
    onRotateLeft: props.onRotateLeft,
    onRotateRight: props.onRotateRight,
    onFlipX: props.onFlipX,
    onFlipY: props.onFlipY,
    onReset: props.onReset,
    onUndo: props.onUndo,
    onRedo: props.onRedo,
    onCropRatio: props.onCropRatio,
    onCropApply: props.onCropApply,
    onCropCancel: props.onCropCancel,
    onCropClear: props.onCropClear,
    onAddText: props.onAddText,
    onTextPatch: props.onTextPatch,
    onTextDelete: props.onTextDelete,
    onAdjustment: props.onAdjustment,
    onResize: props.onResize,
  };
  // Only the capsule row moves to `toolbarContainer`; the tool panels stay in the viewer
  // above the stage unless `panelsContainer` places them elsewhere.
  const capsule = <ImageViewerCapsule {...toolbarProps} />;
  const panels = <ImageViewerToolPanels {...toolbarProps} />;
  const capsuleOut = props.toolbarContainer ? createPortal(capsule, props.toolbarContainer) : null;
  const inViewerCapsule = props.toolbarContainer ? null : capsule;
  const panelsOut = props.panelsContainer ? (
    <>
      <div className="rk-iv-toolbar rk-iv-panels">{inViewerCapsule}</div>
      {createPortal(<div className="rk-iv-toolbar rk-iv-panels">{panels}</div>, props.panelsContainer)}
    </>
  ) : (
    <div className="rk-iv-toolbar rk-iv-panels">
      {panels}
      {inViewerCapsule}
    </div>
  );

  return (
    <div className="rk-iv">
      {panelsOut}
      {capsuleOut}
      <ImageViewerStage
        displayName={displayName}
        displaySrc={displaySrc}
        frameRef={frameRef}
        siblingIndex={siblingIndex}
        siblingTotal={siblingTotal}
        canPrev={canPrev}
        canNext={canNext}
        onPageSibling={props.onPageSibling}
        dragging={dragging}
        panning={panning}
        view={view}
        stageSize={stageSize}
        stageTransform={stageTransform}
        imageSize={imageSize}
        tool={tool}
        edit={edit}
        cropDraft={cropDraft}
        cropRatio={cropRatio}
        filter={filter}
        selectedText={selectedText}
        adapters={adapters}
        onWheel={props.onWheel}
        onPointerDown={props.onPointerDown}
        onPointerMove={props.onPointerMove}
        onPointerStop={props.onPointerStop}
        onDoubleClick={() => {
          if (tool !== "view") return;
          view.mode === "fit" ? props.onActual() : props.onFit();
        }}
        onKeyPan={props.onKeyPan}
        onZoomIn={props.onZoomIn}
        onZoomOut={props.onZoomOut}
        onActual={props.onActual}
        onFit={props.onFit}
        onImageLoad={props.onImageLoad}
        onCropChange={props.onCropChange}
        onTextSelect={props.onTextSelect}
        onTextChange={props.onTextChange}
      />
    </div>
  );
}
