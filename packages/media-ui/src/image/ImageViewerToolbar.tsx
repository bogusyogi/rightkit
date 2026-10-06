import type { ImageAdjustments, ExportResize, TextLayer } from "./imageEdit.js";
import type { MediaAdapters } from "../adapters.js";
import { AdjustPanel, CropPanel, ResizePanel, TextPanel } from "./ImageViewerToolbarPanels.js";
import { Sep, TBtn } from "./toolbar.js";

export type ImageTool = "view" | "crop" | "adjust" | "text" | "resize";

export interface ImageViewerToolbarProps {
  tool: ImageTool;
  /** Read mode — hides the edit/transform controls and tool panels; view + zoom stay. */
  readOnly?: boolean;
  zoomLabel: string;
  fitActive: boolean;
  actualActive: boolean;
  disabled: boolean;
  edited: boolean;
  canUndo: boolean;
  canRedo: boolean;
  flippedX: boolean;
  flippedY: boolean;
  adjustments: ImageAdjustments;
  resize: ExportResize;
  cropRatio: number | null;
  selectedText: TextLayer | null;
  adapters: MediaAdapters;
  onTool: (tool: ImageTool) => void;
  onFit: () => void;
  onActual: () => void;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onZoomPreset: (zoom: number) => void;
  onRotateLeft: () => void;
  onRotateRight: () => void;
  onFlipX: () => void;
  onFlipY: () => void;
  onReset: () => void;
  onUndo: () => void;
  onRedo: () => void;
  onCropRatio: (ratio: number | null) => void;
  onCropApply: () => void;
  onCropCancel: () => void;
  onCropClear: () => void;
  onAddText: () => void;
  onTextPatch: (patch: Partial<TextLayer>) => void;
  onTextDelete: () => void;
  onAdjustment: (key: keyof ImageAdjustments, value: number) => void;
  onResize: (resize: ExportResize) => void;
}

export function ImageViewerToolbar({
  tool,
  readOnly = false,
  zoomLabel,
  fitActive,
  actualActive,
  disabled,
  edited,
  canUndo,
  canRedo,
  flippedX,
  flippedY,
  adjustments,
  resize,
  cropRatio,
  selectedText,
  adapters,
  onTool,
  onFit,
  onActual,
  onZoomIn,
  onZoomOut,
  onZoomPreset,
  onRotateLeft,
  onRotateRight,
  onFlipX,
  onFlipY,
  onReset,
  onUndo,
  onRedo,
  onCropRatio,
  onCropApply,
  onCropCancel,
  onCropClear,
  onAddText,
  onTextPatch,
  onTextDelete,
  onAdjustment,
  onResize,
}: ImageViewerToolbarProps) {
  return (
    <div className="rk-iv-toolbar">
      {/* Tool panels stay in-flow at the top — contextual panels, not the format bar. */}
      {!readOnly && tool === "crop" && (
        <CropPanel
          ratio={cropRatio}
          onRatio={onCropRatio}
          onApply={onCropApply}
          onCancel={onCropCancel}
          onClear={onCropClear}
        />
      )}
      {!readOnly && tool === "adjust" && (
        <AdjustPanel adjustments={adjustments} onChange={onAdjustment} />
      )}
      {!readOnly && tool === "text" && (
        <TextPanel
          selected={selectedText}
          adapters={adapters}
          onAdd={onAddText}
          onPatch={onTextPatch}
          onDelete={onTextDelete}
        />
      )}
      {!readOnly && tool === "resize" && <ResizePanel resize={resize} onChange={onResize} />}

      {/* Single capsule row under any open tool panel; horizontal scroll inside, never wraps. */}
      <div className="rk-iv-bar">
        <div className="rk-iv-capsule">
          <TBtn
            round
            active={fitActive}
            disabled={disabled}
            onRun={onFit}
            title="Fit to window"
            label="fit"
            width={36}
          />
          <TBtn
            round
            active={actualActive}
            disabled={disabled}
            onRun={onActual}
            title="Actual size"
            label="100%"
            width={44}
          />
          {!readOnly && (
            <>
              <Sep />
              <TBtn
                round
                active={tool === "crop"}
                disabled={disabled}
                onRun={() => onTool(tool === "crop" ? "view" : "crop")}
                title="Crop"
                label="crop"
                width={44}
              />
              <TBtn
                round
                active={tool === "adjust"}
                disabled={disabled}
                onRun={() => onTool(tool === "adjust" ? "view" : "adjust")}
                title="Adjust"
                label="adjust"
                width={54}
              />
              <TBtn
                round
                active={tool === "text"}
                disabled={disabled}
                onRun={() => onTool(tool === "text" ? "view" : "text")}
                title="Add text"
                label="text"
                width={42}
              />
              <TBtn
                round
                active={tool === "resize"}
                disabled={disabled}
                onRun={() => onTool(tool === "resize" ? "view" : "resize")}
                title="Export size"
                label="size"
                width={42}
              />
              <Sep />
              <TBtn
                round
                disabled={disabled || !canUndo}
                onRun={onUndo}
                title="Undo last edit"
                label="undo"
                width={46}
              />
              <TBtn
                round
                disabled={disabled || !canRedo}
                onRun={onRedo}
                title="Redo edit"
                label="redo"
                width={42}
              />
              <TBtn
                round
                active={edited}
                disabled={disabled}
                onRun={onReset}
                title="Reset edits and view"
                label="reset"
                width={48}
              />
            </>
          )}
          <Sep />
          <TBtn round disabled={disabled} onRun={onZoomOut} title="Zoom out" label="-" />
          <ZoomSelect disabled={disabled} zoomLabel={zoomLabel} onZoomPreset={onZoomPreset} />
          <TBtn round disabled={disabled} onRun={onZoomIn} title="Zoom in" label="+" />
          {!readOnly && (
            <>
              <Sep />
              <TBtn round disabled={disabled} onRun={onRotateLeft} title="Rotate left" label="↺" />
              <TBtn round disabled={disabled} onRun={onRotateRight} title="Rotate right" label="↻" />
              <TBtn
                round
                active={flippedX}
                disabled={disabled}
                onRun={onFlipX}
                title="Flip horizontal"
                label="H↔"
                width={34}
              />
              <TBtn
                round
                active={flippedY}
                disabled={disabled}
                onRun={onFlipY}
                title="Flip vertical"
                label="V↕"
                width={34}
              />
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function zoomPresetValue(label: string): string {
  const numeric = Number(label.replace(/%/g, ""));
  if (!Number.isFinite(numeric)) return "100";
  return String(Math.max(1, Math.round(numeric)));
}

const ZOOM_PRESETS = ["25", "50", "75", "100", "125", "150", "200", "400"];

function ZoomSelect({
  disabled,
  zoomLabel,
  onZoomPreset,
}: {
  disabled: boolean;
  zoomLabel: string;
  onZoomPreset: (zoom: number) => void;
}) {
  const value = zoomPresetValue(zoomLabel);
  return (
    <select
      aria-label="Zoom percent"
      title={`Zoom ${zoomLabel}`}
      disabled={disabled}
      value={value}
      onChange={(event) => onZoomPreset(Number(event.target.value))}
      className="rk-iv-input rk-iv-zoom"
    >
      {ZOOM_PRESETS.map((preset) => (
        <option key={preset} value={preset}>
          {preset}%
        </option>
      ))}
      {!ZOOM_PRESETS.includes(value) && <option value={value}>{zoomLabel}</option>}
    </select>
  );
}
