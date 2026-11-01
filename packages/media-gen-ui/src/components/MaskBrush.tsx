import { useCallback, useEffect, useRef, useState } from 'react';
import { Button, Notice, Slider } from '@rightkit/react-shell/react';
import { canvasToArrayBuffer, validateExportCanvasSize } from '@rightkit/media-ui';
import { isMaskEmpty, maskRgba, toSourcePoint, type MaskStroke, type MaskTool } from '../lib/mask-export';

interface Props {
  /** Displayable URL of the source image (the first reference image). */
  src: string;
  /** Saves the exported PNG (white = repaint, source pixel size) and resolves to its local file path. */
  onSave: (png: Blob, width: number, height: number) => Promise<string>;
  /** Called with the saved mask path, or null when the mask is cleared. */
  onChange: (path: string | null) => void;
  disabled?: boolean;
}

/** Overlay canvas on the source image: brush, eraser, size, invert, clear. Exports a B/W PNG at the source's pixel size. */
// RightKit ruling 12b1a381: N/A valid product UI
export function MaskBrush({ src, onSave, onChange, disabled }: Props) {
  const canvas = useRef<HTMLCanvasElement | null>(null);
  const strokes = useRef<MaskStroke[]>([]);
  const drawing = useRef<MaskStroke | null>(null);
  const [dims, setDims] = useState<{ w: number; h: number } | null>(null);
  const [tool, setTool] = useState<MaskTool>('brush');
  const [size, setSize] = useState(40);
  const [invert, setInvert] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [version, setVersion] = useState(0);

  useEffect(() => {
    strokes.current = [];
    setDims(null);
    setNote(null);
    onChange(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src]);

  // Draw the overlay in source pixels: repaint area as translucent red.
  useEffect(() => {
    const c = canvas.current;
    if (!c || !dims) return;
    c.width = dims.w;
    c.height = dims.h;
    const ctx = c.getContext('2d');
    if (!ctx) return;
    const rgba = maskRgba(strokes.current, dims.w, dims.h, invert);
    const img = ctx.createImageData(dims.w, dims.h);
    for (let i = 0; i < rgba.length; i += 4) {
      img.data[i] = 255;
      img.data[i + 1] = 40;
      img.data[i + 2] = 80;
      img.data[i + 3] = rgba[i] === 255 ? 110 : 0;
    }
    ctx.putImageData(img, 0, 0);
  }, [dims, invert, version]);

  const pointAt = useCallback((e: React.PointerEvent<HTMLCanvasElement>) => {
    const c = canvas.current!;
    return toSourcePoint(e.clientX, e.clientY, c.getBoundingClientRect(), c.width, c.height);
  }, []);

  const displayScale = () => {
    const c = canvas.current;
    return c && c.clientWidth ? c.width / c.clientWidth : 1;
  };

  const down = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (disabled || !dims) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    // Slider size is in display pixels so the brush looks the same on any image resolution.
    const s: MaskStroke = { tool, size: size * displayScale(), points: [pointAt(e)] };
    drawing.current = s;
    strokes.current.push(s);
    setVersion((v) => v + 1);
  };
  const move = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drawing.current) return;
    drawing.current.points.push(pointAt(e));
    setVersion((v) => v + 1);
  };
  const up = () => {
    drawing.current = null;
  };

  const clear = () => {
    strokes.current = [];
    setInvert(false);
    setNote(null);
    setVersion((v) => v + 1);
    onChange(null);
  };

  const save = async () => {
    if (!dims) return;
    if (isMaskEmpty(strokes.current) && !invert) {
      setNote('Paint the area to repaint first.');
      return;
    }
    setBusy(true);
    setNote(null);
    try {
      validateExportCanvasSize({ width: dims.w, height: dims.h });
      const rgba = maskRgba(strokes.current, dims.w, dims.h, invert);
      const off = document.createElement('canvas');
      off.width = dims.w;
      off.height = dims.h;
      const ctx = off.getContext('2d');
      if (!ctx) throw new Error('No canvas available.');
      const img = ctx.createImageData(dims.w, dims.h);
      img.data.set(rgba);
      ctx.putImageData(img, 0, 0);
      const blob = new Blob([await canvasToArrayBuffer(off, 'image/png')], { type: 'image/png' });
      const path = await onSave(blob, dims.w, dims.h);
      onChange(path);
      setNote(`Mask saved (${dims.w}×${dims.h}).`);
    } catch (e) {
      setNote(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="gen-mask" role="group" aria-label="Mask brush">
      <div className="gen-refs-head">
        <span className="eyebrow">Mask · white repaints</span>
        <Button type="button" variant={tool === 'brush' ? 'secondary' : 'ghost'} aria-pressed={tool === 'brush'} onClick={() => setTool('brush')}>Brush</Button>
        <Button type="button" variant={tool === 'eraser' ? 'secondary' : 'ghost'} aria-pressed={tool === 'eraser'} onClick={() => setTool('eraser')}>Eraser</Button>
        <label>
          Size <Slider min={4} max={160} value={size} label="Brush size" onChange={setSize} />
        </label>
        <Button type="button" variant="ghost" aria-pressed={invert} onClick={() => setInvert((v) => !v)}>Invert</Button>
        <Button type="button" variant="ghost" onClick={clear}>Clear</Button>
        <Button type="button" variant="secondary" disabled={busy || disabled || !dims} onClick={() => void save()}>{busy ? 'Saving…' : 'Use mask'}</Button>
      </div>
      <div style={{ position: 'relative', display: 'inline-block', maxWidth: '100%' }}>
        <img
          src={src}
          alt="Source image to mask"
          style={{ display: 'block', maxWidth: '100%', maxHeight: 360 }}
          onLoad={(e) => setDims({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })}
        />
        <canvas
          ref={canvas}
          aria-label="Mask overlay"
          style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', touchAction: 'none', cursor: 'crosshair' }}
          onPointerDown={down}
          onPointerMove={move}
          onPointerUp={up}
          onPointerCancel={up}
        />
      </div>
      {note ? <Notice tone="warn" className="li-warn">{note}</Notice> : null}
    </div>
  );
}
