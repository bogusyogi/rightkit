import { Field, Input } from "@rightkit/react-shell/react";
import { useState } from "react";
import { errorText } from "./adapter.js";

import { editCaption, removeCaption } from "./actions.js";
import type { EditSession } from "./adapter.js";
import type { EditorCaption, EditorModel } from "./model.js";

export function CaptionInspector({ model, session, onCommitted, onError }: {
  model: EditorModel; session: EditSession; onCommitted: () => void; onError: (s: string) => void;
}) {
  const [id, setId] = useState("");
  const cue = model.captions.find((c) => c.id === id) ?? model.captions[0];
  return <div className="fe-inspect" aria-label="Caption corrections">
    <Field label="Caption">{(ids) => (<select {...ids} className="rk-input cr-select" aria-label="Caption to correct" value={cue?.id ?? ""}
        onChange={(e) => setId(e.target.value)}>
        {model.captions.map((c) => <option key={c.id} value={c.id}>{c.text}</option>)}
      </select>)}</Field>
    {cue ? <CaptionFields key={`${cue.id}:${model.revisionCursor}`} cue={cue} fps={model.fps}
      onSave={async (next) => {
        try { await session.apply(editCaption(cue.id, next), "Caption correction"); onCommitted(); }
        catch (e) { onError(errorText(e)); onCommitted(); }
      }}
      onRestore={async () => {
        try { await session.apply(removeCaption(cue.id), "Restore original caption"); onCommitted(); }
        catch (e) { onError(errorText(e)); onCommitted(); }
      }} /> : <p className="cr-muted">Transcribe footage to edit captions.</p>}
  </div>;
}

function CaptionFields({ cue, fps, onSave, onRestore }: {
  cue: EditorCaption; fps: number; onSave: (c: EditorCaption) => Promise<void>; onRestore: () => Promise<void>;
}) {
  const [draft, setDraft] = useState(cue);
  const number = (label: string, value: number, update: (v: number) => void) =>
    <Field label={label}>{(ids) => (<Input {...ids} type="number" step="0.01" aria-label={label} value={value}
      onChange={(e) => update(Number(e.target.value))} />)}</Field>;
  return <>
    <Field label="Caption text">{(ids) => (<textarea {...ids} className="rk-input" aria-label="Caption text" maxLength={160}
      value={draft.text} onChange={(e) => setDraft({ ...draft, text: e.target.value })} />)}</Field>
    {number("Caption start (s)", draft.start_frame / fps, (v) => setDraft({ ...draft, start_frame: Math.round(v * fps) }))}
    {number("Caption duration (s)", draft.frames / fps, (v) => setDraft({ ...draft, frames: Math.round(v * fps) }))}
    {number("Caption horizontal position (%)", draft.position.x / 10, (v) => setDraft({ ...draft, position: { ...draft.position, x: Math.round(v * 10) } }))}
    {number("Caption vertical position (%)", draft.position.y / 10, (v) => setDraft({ ...draft, position: { ...draft.position, y: Math.round(v * 10) } }))}
    <button type="button" className="rk-button" onClick={() => void onSave(draft)}>Save caption</button>
    {cue.corrected ? <button type="button" className="rk-button" onClick={() => void onRestore()}>Restore original</button> : null}
  </>;
}
