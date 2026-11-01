import { Button, Notice } from '@rightkit/react-shell/react';
import { nextStep, splitError } from '../lib/gen-params';

/** An engine error with its code, message and a concrete next step. */
export function ErrorNote({ code, message, onOpenKeys, onFallback }: { code?: string | null; message: string; onOpenKeys?: () => void; onFallback?: () => void }) {
  const split = code ? { code, message } : splitError(message);
  const step = nextStep(split.code);
  return (
    <Notice tone="bad" className="gen-err">
      <div>{split.code ? <span className="mono gen-err-code">{split.code}</span> : null}{split.message || message}</div>
      {step ? (
        <div className="gen-err-next">
          {step.text}{' '}
          {step.action && onOpenKeys ? <Button type="button" variant="ghost" size="sm" onClick={onOpenKeys}>Open Models &amp; Keys</Button> : null}
        </div>
      ) : null}
      {split.code === 'voice_unavailable' && onFallback ? (
        <div><Button type="button" size="sm" onClick={onFallback}>Use WaveSpeed ElevenLabs Turbo v2.5 instead</Button></div>
      ) : null}
    </Notice>
  );
}
