import { useEffect, useRef, useState } from 'react';
import { Button, Notice } from '@rightkit/react-shell/react';
import { MaskPainter, type MaskPainterHandle } from '@rightkit/media-ui';

interface Props {
  /** Displayable URL of the source image (the first reference image). */
  src: string;
  /** Saves the exported PNG (white = repaint, source pixel size) and resolves to its local file path. */
  onSave: (png: Blob) => Promise<string>;
  /** Called with the saved mask path, or null when the mask is cleared or the source changes. */
  onChange: (path: string | null) => void;
  disabled?: boolean;
}

/**
 * The generate form's mask step: @rightkit/media-ui MaskPainter (brush, eraser, size, clear) plus
 * invert and "Use mask", which exports a black-and-white PNG at the source's pixel size and saves it.
 */
export function MaskEditor({ src, onSave, onChange, disabled }: Props) {
  const painter = useRef<MaskPainterHandle | null>(null);
  const [hasMask, setHasMask] = useState(false);
  const [invert, setInvert] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    setInvert(false);
    setNote(null);
    onChange(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src]);

  const save = async () => {
    if (!painter.current) return;
    if (!painter.current.hasMask() && !invert) {
      setNote('Paint the area to repaint first.');
      return;
    }
    setBusy(true);
    setNote(null);
    try {
      const png = await painter.current.exportPng({ invert });
      onChange(await onSave(png));
      setNote('Mask saved.');
    } catch (e) {
      setNote(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="gen-mask" role="group" aria-label="Mask">
      <div className="gen-refs-head">
        <span className="eyebrow">Mask · {invert ? 'unpainted area repaints' : 'painted area repaints'}</span>
        <Button type="button" variant="ghost" aria-pressed={invert} onClick={() => setInvert((v) => !v)}>Invert</Button>
        <Button type="button" variant="secondary" disabled={busy || disabled || (!hasMask && !invert)} onClick={() => void save()}>
          {busy ? 'Saving…' : 'Use mask'}
        </Button>
      </div>
      <MaskPainter
        ref={painter}
        src={src}
        onMaskChange={(painted) => {
          setHasMask(painted);
          if (!painted) onChange(null);
        }}
      />
      {note ? <Notice tone="warn" className="li-warn">{note}</Notice> : null}
    </div>
  );
}
