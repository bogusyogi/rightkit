import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type SyntheticEvent,
} from "react";
import { exportMaskPng, maskHasPaint, type MaskExportOptions } from "./mask.js";

export type MaskTool = "brush" | "erase";

export interface MaskPainterLabels {
  region: string;
  brush: string;
  erase: string;
  size: string;
  clear: string;
}

export const DEFAULT_MASK_PAINTER_LABELS: MaskPainterLabels = {
  region: "Mask painter",
  brush: "Brush",
  erase: "Erase",
  size: "Brush size",
  clear: "Clear mask",
};

export interface MaskPainterProps {
  /** Image to paint over. The mask has the image's natural resolution. */
  src: string;
  /** Initial brush diameter in image pixels. Default 32. */
  brushSize?: number;
  /** Initial tool. Default "brush". The toolbar changes it. */
  tool?: MaskTool;
  /** Called after each stroke, after a clear, and after the image loads, with whether any pixel is painted. */
  onMaskChange?: (hasMask: boolean) => void;
  /** Colour drawn over the painted area on screen only; it never reaches the exported mask. */
  tint?: string;
  labels?: Partial<MaskPainterLabels>;
  className?: string;
}

export interface MaskPainterHandle {
  /** PNG of the mask at image resolution. Rejects before the image has loaded. */
  exportPng(options?: MaskExportOptions): Promise<Blob>;
  /** True when at least one pixel is painted. */
  hasMask(): boolean;
  clear(): void;
}

interface Point {
  x: number;
  y: number;
}

interface ActiveStroke {
  pointerId: number;
  last: Point;
  erase: boolean;
}

const DEFAULT_BRUSH = 32;
const MAX_BRUSH = 256;
const DEFAULT_TINT = "rgba(255, 64, 64, 0.5)";

function clampBrush(size: number): number {
  return Number.isFinite(size) ? Math.max(1, Math.min(MAX_BRUSH, Math.round(size))) : DEFAULT_BRUSH;
}

function drawSegment(ctx: CanvasRenderingContext2D, from: Point, to: Point, size: number, erase: boolean): void {
  ctx.save();
  ctx.globalCompositeOperation = erase ? "destination-out" : "source-over";
  ctx.strokeStyle = "#fff";
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  ctx.lineWidth = size;
  ctx.beginPath();
  ctx.moveTo(from.x, from.y);
  ctx.lineTo(to.x, to.y);
  ctx.stroke();
  ctx.restore();
}

function drawDot(ctx: CanvasRenderingContext2D, at: Point, size: number, erase: boolean): void {
  ctx.save();
  ctx.globalCompositeOperation = erase ? "destination-out" : "source-over";
  ctx.fillStyle = "#fff";
  ctx.beginPath();
  ctx.arc(at.x, at.y, size / 2, 0, Math.PI * 2);
  ctx.fill();
  ctx.restore();
}

/**
 * Brush and erase painting over an image, with a PNG mask export for inpainting. The mask lives on an offscreen
 * canvas at the image's natural size; the visible canvas only previews it, so the export is exact. Plain canvas:
 * no dependency.
 */
export const MaskPainter = forwardRef<MaskPainterHandle, MaskPainterProps>(function MaskPainter(props, ref) {
  const { src, onMaskChange, className } = props;
  const labels: MaskPainterLabels = { ...DEFAULT_MASK_PAINTER_LABELS, ...props.labels };
  const [tool, setTool] = useState<MaskTool>(props.tool ?? "brush");
  const [brushSize, setBrushSize] = useState<number>(clampBrush(props.brushSize ?? DEFAULT_BRUSH));
  const displayRef = useRef<HTMLCanvasElement | null>(null);
  const maskRef = useRef<HTMLCanvasElement | null>(null);
  const strokeRef = useRef<ActiveStroke | null>(null);
  const tintRef = useRef(props.tint ?? DEFAULT_TINT);
  const brushRef = useRef(brushSize);
  const toolRef = useRef(tool);
  tintRef.current = props.tint ?? DEFAULT_TINT;
  brushRef.current = brushSize;
  toolRef.current = tool;

  /** Repaints the preview: the mask, tinted where painted. */
  const redraw = useCallback((): void => {
    const display = displayRef.current;
    const mask = maskRef.current;
    const ctx = display?.getContext("2d");
    if (!display || !mask || !ctx) return;
    ctx.clearRect(0, 0, display.width, display.height);
    ctx.drawImage(mask, 0, 0);
    ctx.save();
    ctx.globalCompositeOperation = "source-in";
    ctx.fillStyle = tintRef.current;
    ctx.fillRect(0, 0, display.width, display.height);
    ctx.restore();
  }, []);

  const readMaskHasPaint = useCallback((): boolean => {
    const mask = maskRef.current;
    const ctx = mask?.getContext("2d");
    if (!mask || !ctx) return false;
    return maskHasPaint(ctx.getImageData(0, 0, mask.width, mask.height).data);
  }, []);

  const clearMask = useCallback((): void => {
    const mask = maskRef.current;
    mask?.getContext("2d")?.clearRect(0, 0, mask.width, mask.height);
    redraw();
    onMaskChange?.(false);
  }, [onMaskChange, redraw]);

  useImperativeHandle(ref, () => ({
    exportPng(options) {
      const mask = maskRef.current;
      if (!mask) return Promise.reject(new Error("mask image is not loaded"));
      return exportMaskPng(mask, options);
    },
    hasMask: readMaskHasPaint,
    clear: clearMask,
  }), [clearMask, readMaskHasPaint]);

  // A new image starts with an empty mask at its natural size.
  const handleLoad = (event: SyntheticEvent<HTMLImageElement>): void => {
    const image = event.currentTarget;
    const width = image.naturalWidth;
    const height = image.naturalHeight;
    if (width === 0 || height === 0) return;
    const mask = document.createElement("canvas");
    mask.width = width;
    mask.height = height;
    maskRef.current = mask;
    const display = displayRef.current;
    if (display) {
      display.width = width;
      display.height = height;
    }
    redraw();
    onMaskChange?.(false);
  };

  useEffect(() => {
    redraw();
  }, [props.tint, redraw]);

  const pointAt = (event: ReactPointerEvent<HTMLCanvasElement>): Point | null => {
    const canvas = displayRef.current;
    if (!canvas) return null;
    const rect = canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return null;
    return {
      x: ((event.clientX - rect.left) * canvas.width) / rect.width,
      y: ((event.clientY - rect.top) * canvas.height) / rect.height,
    };
  };

  const handlePointerDown = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    const mask = maskRef.current;
    const ctx = mask?.getContext("2d");
    const at = pointAt(event);
    if (!mask || !ctx || !at || event.button !== 0) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    const erase = toolRef.current === "erase";
    strokeRef.current = { pointerId: event.pointerId, last: at, erase };
    drawDot(ctx, at, brushRef.current, erase);
    redraw();
  };

  const handlePointerMove = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    const stroke = strokeRef.current;
    const ctx = maskRef.current?.getContext("2d");
    if (!stroke || !ctx || stroke.pointerId !== event.pointerId) return;
    const at = pointAt(event);
    if (!at) return;
    drawSegment(ctx, stroke.last, at, brushRef.current, stroke.erase);
    stroke.last = at;
    redraw();
  };

  const finishStroke = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    const stroke = strokeRef.current;
    if (!stroke || stroke.pointerId !== event.pointerId) return;
    strokeRef.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    onMaskChange?.(readMaskHasPaint());
  };

  return (
    <div className={["rk-mask", className].filter(Boolean).join(" ")} role="group" aria-label={labels.region}>
      <div className="rk-mask-toolbar">
        <button type="button" aria-pressed={tool === "brush"} onClick={() => setTool("brush")}>
          {labels.brush}
        </button>
        <button type="button" aria-pressed={tool === "erase"} onClick={() => setTool("erase")}>
          {labels.erase}
        </button>
        <label className="rk-mask-size">
          <span>{labels.size}</span>
          <input
            type="range"
            min={1}
            max={MAX_BRUSH}
            value={brushSize}
            onChange={(event) => setBrushSize(clampBrush(Number(event.currentTarget.value)))}
          />
        </label>
        <button type="button" onClick={clearMask}>
          {labels.clear}
        </button>
      </div>
      <div className="rk-mask-stage">
        <img className="rk-mask-image" src={src} alt="" draggable={false} onLoad={handleLoad} />
        <canvas
          ref={displayRef}
          className="rk-mask-canvas"
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={finishStroke}
          onPointerCancel={finishStroke}
        />
      </div>
    </div>
  );
});
