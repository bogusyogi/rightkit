/**
 * Selection inspector: continuous controls preview locally and commit once on
 * release/blur through `useInspectorGestureTransaction` (adapted from
 * hyperframes). Every control maps to a real `rightkit_timeline::Action` or is
 * rendered disabled with the capability reason.
 */
import { Checkbox, Field, Input } from "@rightkit/react-shell/react";
import { errorText } from "./adapter.js";
import { useEffect, useMemo, useState } from "react";

import { engineCall } from "../lib/engine.js";
import { decimatePeaks, loudnessToOpacity } from "./vendor/hyperframes/audioWaveformPeaks.js";
import { ticksToSeconds, type TitleTemplate } from "../lib/project.js";
import { useInspectorGestureDraft } from "./vendor/hyperframes/useInspectorGestureTransaction.js";
import type { EditAction } from "./actions.js";
import {
  editTitle,
  linkNarration,
  setGain,
  setMute,
  positionTitle,
  swapTake,
  setUnder,
  trimAudioClip,
  trimClip,
  trimTitle,
  unlinkNarration,
  type UnderMode,
} from "./actions.js";
import type { EditorClip, EditorModel } from "./model.js";
import { supports, unsupported, type CapabilityReport } from "./capabilities.js";
import type { EditSession } from "./adapter.js";
import { ColourInspector } from "./ColourInspector.js";

export function Inspector({
  model,
  selection,
  session,
  caps,
  dir,
  playheadS,
  onCommitted,
  onError,
}: {
  model: EditorModel;
  selection: ReadonlySet<string>;
  session: EditSession;
  caps: CapabilityReport | null;
  /** Project dir for the media.waveform call. */
  dir: string;
  /** Timeline playhead in seconds; drives the colour scope sample frame. */
  playheadS?: number;
  onCommitted: () => void;
  onError: (message: string) => void;
}) {
  const clip = useMemo(() => {
    for (const id of selection) {
      const hit = model.lanes.flatMap((l) => l.clips).find((c) => c.id === id);
      if (hit) return hit;
    }
    return null;
  }, [model, selection]);

  const commit = async (actions: EditAction[]) => {
    try {
      await session.apply(actions.filter(Boolean), "Inspector edit");
      onCommitted();
    } catch (e) {
      onError(errorText(e));
      onCommitted();
    }
  };

  if (selection.size === 0) {
    return (
      <p className="cr-muted" style={{ margin: 0, fontSize: 12 }}>
        Select a clip on the timeline to edit its trim, audio, or title.
      </p>
    );
  }
  if (selection.size > 1) {
    return (
      <p className="cr-muted" style={{ margin: 0, fontSize: 12 }}>
        {selection.size} clips selected — drag to move together; inspect edits one clip.
      </p>
    );
  }
  if (!clip) return null;

  return (
    <div className="fe-inspect" aria-label="Clip inspector">
      {clip.ref.kind === "video" && (
        <VideoInspector key={`${clip.id}:${model.revisionCursor}`} clip={clip} model={model} commit={commit} caps={caps} dir={dir} playheadS={playheadS} />
      )}
      {clip.ref.kind === "audio" && (
        <AudioInspector key={`${clip.id}:${model.revisionCursor}`} clip={clip} model={model} commit={commit} caps={caps} dir={dir} />
      )}
      {clip.ref.kind === "title" && (
        <TitleInspector key={`${clip.id}:${model.revisionCursor}`} clip={clip} model={model} commit={commit} caps={caps} />
      )}
    </div>
  );
}

function SecondsInput({
  label,
  seconds,
  onCommit,
}: {
  label: string;
  seconds: number;
  onCommit: (next: number) => void;
}) {
  const { draft, setDraft, transaction } = useInspectorGestureDraft<number>({
    sourceValue: seconds,
    onPreview: () => undefined,
    onCommit: (v) => onCommit(v),
  });
  return (
    <Field label={label}>{(ids) => (<Input {...ids}
        inputMode="decimal"
        value={String(draft.toFixed(2))}
        onFocus={() => transaction.begin()}
        onChange={(e) => {
          const v = Number(e.target.value);
          if (Number.isFinite(v)) {
            setDraft(v);
            transaction.preview(v);
          }
        }}
        onBlur={() => transaction.settle()}
        onKeyDown={(e) => {
          if (e.key === "Enter") transaction.settle();
          if (e.key === "Escape") transaction.cancel();
        }}
      />)}</Field>
  );
}

function VideoInspector({
  clip,
  model,
  commit,
  caps,
  dir,
  playheadS,
}: {
  clip: EditorClip;
  model: EditorModel;
  commit: (a: EditAction[]) => Promise<void>;
  caps: CapabilityReport | null;
  /** Project dir for the colour calls. */
  dir: string;
  playheadS?: number;
}) {
  if (clip.ref.kind !== "video") return null;
  const clipIndex = clip.ref.index;
  const inS = ticksToSeconds(clip.ref.clip.source_in);
  const outS = ticksToSeconds(clip.ref.clip.source_out);
  // Source time under the playhead when it is inside this clip; otherwise source-in.
  const scopeS =
    playheadS !== undefined && playheadS >= clip.start && playheadS <= clip.start + clip.duration
      ? Math.min(outS, Math.max(inS, inS + (playheadS - clip.start)))
      : inS;
  const srcDur = model.sourceDurationS.get(clip.ref.clip.source) ?? outS;
  const [take, setTake] = useState(clip.ref.clip.source);
  const [takeIn, setTakeIn] = useState(inS);
  const [takeOut, setTakeOut] = useState(outS);
  return (
    <>
      <div className="fe-inspect__row">
        <SecondsInput
          label="Source in (s)"
          seconds={inS}
          onCommit={(v) =>
            void commit([trimClip(clip.id, Math.max(0, Math.min(v, outS - 0.016)), outS)])
          }
        />
        <SecondsInput
          label="Source out (s)"
          seconds={outS}
          onCommit={(v) =>
            void commit([trimClip(clip.id, inS, Math.min(srcDur, Math.max(v, inS + 0.016)))])
          }
        />
        <Field label="Track position">{(ids) => (<span {...ids} style={{ fontSize: 13 }}>
            clip {clipIndex + 1} · {clip.start.toFixed(2)}s–{(clip.start + clip.duration).toFixed(2)}s
          </span>)}</Field>
      </div>
      <GatedRow cap="edit.split" caps={caps} label="Split clip" />
      <GatedRow cap="edit.slip" caps={caps} label="Slip source range" />
      <Field label="Clip gain (dB)">{() => (<SecondsInput label="Gain" seconds={(model.corrections.audio[clip.id]?.gain ?? 0) / 1000}
          onCommit={(v) => void commit([setGain(clip.id, v)])} />)}</Field>
      <MuteControl clipId={clip.id} model={model} commit={commit} />
      <Field label="Alternate take">{(ids) => (<select {...ids} className="rk-input cr-select" value={take} aria-label="Alternate take source"
          onChange={(e) => setTake(e.target.value)}>
          {model.sources.filter((s) => s.info.video).map((s) =>
            <option key={s.info.id} value={s.info.id}>{s.name ?? s.info.id.slice(0, 8)}</option>)}
        </select>)}</Field>
      <SecondsInput label="Take in (s)" seconds={takeIn} onCommit={setTakeIn} />
      <SecondsInput label="Take out (s)" seconds={takeOut} onCommit={setTakeOut} />
      <button type="button" className="rk-button" onClick={() => void commit([swapTake(clip.id, take, takeIn, takeOut)])}>
        Use this take
      </button>
      {/* Scopes sample the source frame under the playhead, or source-in when the playhead is outside the clip. */}
      <ColourInspector dir={dir} sourceId={clip.ref.clip.source} atMs={Math.round(scopeS * 1000)} />
    </>
  );
}

/**
 * Per-clip waveform over the source's decoded peaks. Rendered only when the
 * engine advertises `media.waveform`; otherwise the gate reason shows.
 */
function Waveform({
  dir,
  source,
  inS,
  outS,
  srcDurS,
}: {
  dir: string;
  source: string;
  inS: number;
  outS: number;
  /** Full source duration in seconds (peaks span the whole file). */
  srcDurS: number;
}) {
  const [peaks, setPeaks] = useState<number[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setPeaks(null);
    setErr(null);
    engineCall<{ peaks?: number[] }>("media.waveform", { dir, source, bars: 600 })
      .then((r) => !cancelled && setPeaks(r.peaks ?? []))
      .catch((e) => !cancelled && setErr(errorText(e)));
    return () => {
      cancelled = true;
    };
  }, [dir, source]);
  if (err) return <span className="cr-muted" style={{ fontSize: 12 }}>{err}</span>;
  if (!peaks) return <span className="cr-muted" style={{ fontSize: 12 }}>Loading peaks…</span>;
  const dur = srcDurS > 0 ? srcDurS : 1;
  const bars = decimatePeaks(peaks, inS / dur, outS / dur, 96);
  return (
    <div
      role="img"
      aria-label="Clip waveform"
      style={{ display: "flex", height: 32, alignItems: "flex-end", gap: 1 }}
    >
      {bars.map((p, i) => (
        <div
          key={i}
          style={{
            width: 4,
            borderRadius: 2,
            height: `${Math.max(8, p * 100)}%`,
            background: "var(--rk-accent)",
            opacity: loudnessToOpacity(p),
          }}
        />
      ))}
    </div>
  );
}

function AudioInspector({
  clip,
  model,
  commit,
  caps,
  dir,
}: {
  clip: EditorClip;
  model: EditorModel;
  commit: (a: EditAction[]) => Promise<void>;
  caps: CapabilityReport | null;
  dir: string;
}) {
  if (clip.ref.kind !== "audio") return null;
  const c = clip.ref.clip;
  const gainDb = c.gain / 1000;
  const under: UnderMode = c.under as UnderMode;
  const videoTargets = model.lanes
    .filter((l) => l.kind === "video")
    .flatMap((l) => l.clips);
  const [duckDb, setDuckDb] = useState(12);

  const { draft, setDraft, transaction } = useInspectorGestureDraft<number>({
    sourceValue: gainDb,
    onPreview: () => undefined,
    onCommit: (v) => void commit([setGain(clip.id, v)]),
  });

  return (
    <>
      <div className="fe-inspect">
        <MuteControl clipId={clip.id} model={model} commit={commit} />
        <Field label={`Gain ${draft.toFixed(1)} dB`}>{(ids) => (<Input {...ids}
            type="range"
            min={-60}
            max={12}
            step={0.5}
            value={draft}
            aria-label="Clip gain"
            className="fe-inspect__range"
            onPointerDown={() => transaction.begin()}
            onChange={(e) => {
              const v = Number(e.target.value);
              setDraft(v);
              transaction.preview(v);
            }}
            onPointerUp={() => transaction.settle()}
            onKeyDown={(e) => {
              if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
                transaction.begin();
                const v = draft + (e.key === "ArrowRight" ? 0.5 : -0.5);
                setDraft(v);
                transaction.preview(v);
                transaction.settle();
              }
            }}
          />)}</Field>
        <Field label="Audio underneath">{(ids) => (<select {...ids}
            className="rk-input cr-select"
            value={under.mode}
            aria-label="Under audio mode"
            onChange={(e) => {
              const mode = e.target.value;
              const next: UnderMode =
                mode === "mute" ? { mode: "mute" }
                : mode === "duck" ? { mode: "duck", amount: Math.round(duckDb * 1000) }
                : { mode: "keep" };
              void commit([setUnder(clip.id, next)]);
            }}
          >
            <option value="keep">Keep</option>
            <option value="duck">Duck</option>
            <option value="mute">Mute</option>
          </select>)}</Field>
        {under.mode === "duck" && (
          <Field label="Duck amount (dB)">{(ids) => (<Input {...ids}
              inputMode="decimal"
              value={duckDb}
              onChange={(e) => setDuckDb(Number(e.target.value) || 0)}
              onBlur={() =>
                void commit([setUnder(clip.id, { mode: "duck", amount: Math.round(duckDb * 1000) })])
              }
            />)}</Field>
        )}
        <Field label="Linked video clip">{(ids) => (<select {...ids}
            className="rk-input cr-select"
            value={c.link ?? ""}
            aria-label="Linked video clip"
            onChange={(e) => {
              const v = e.target.value;
              void commit(v ? [linkNarration(clip.id, v)] : [unlinkNarration(clip.id)]);
            }}
          >
            <option value="">None</option>
            {videoTargets.map((v) => (
              <option key={v.id} value={v.id}>
                {v.label}
              </option>
            ))}
          </select>)}</Field>
      </div>
      <div className="fe-inspect__row">
        <SecondsInput
          label="Trim in (s)"
          seconds={ticksToSeconds(c.trim_in)}
          onCommit={(v) =>
            void commit([
              trimAudioClip(clip.id, Math.max(0, Math.min(v, ticksToSeconds(c.trim_out) - 0.016)), ticksToSeconds(c.trim_out)),
            ])
          }
        />
        <SecondsInput
          label="Trim out (s)"
          seconds={ticksToSeconds(c.trim_out)}
          onCommit={(v) =>
            void commit([
              trimAudioClip(clip.id, ticksToSeconds(c.trim_in), Math.max(v, ticksToSeconds(c.trim_in) + 0.016)),
            ])
          }
        />
        <Field label="Waveform">{() => (supports("media.waveform", caps) ? (
            <Waveform
              dir={dir}
              source={c.source}
              inS={ticksToSeconds(c.trim_in)}
              outS={ticksToSeconds(c.trim_out)}
              srcDurS={model.sourceDurationS.get(c.source) ?? ticksToSeconds(c.trim_out)}
            />
          ) : (
            <span className="cr-muted" style={{ fontSize: 12 }}>
              {unsupported("media.waveform").reason}
            </span>
          ))}</Field>
      </div>
    </>
  );
}

function TitleInspector({
  clip,
  model,
  commit,
  caps,
}: {
  clip: EditorClip;
  model: EditorModel;
  commit: (a: EditAction[]) => Promise<void>;
  caps: CapabilityReport | null;
}) {
  void caps;
  if (clip.ref.kind !== "title") return null;
  const c = clip.ref.clip;
  const [template, setTemplate] = useState<TitleTemplate>(c.template);
  const fadeIn: number = c.fade_in_frames ?? 0;
  const fadeOut: number = c.fade_out_frames ?? 0;
  const text =
    template.template === "lower_third" ? template.title : template.text;
  return (
    <div className="fe-inspect">
      <Field label="Template">{(ids) => (<select {...ids}
          className="rk-input cr-select"
          value={template.template}
          aria-label="Title template"
          onChange={(e) => {
            const kind = e.target.value;
            const next: TitleTemplate =
              kind === "lower_third"
                ? { template: "lower_third", title: text }
                : kind === "centered"
                  ? { template: "centered", text }
                  : { template: "caption", text };
            setTemplate(next);
            void commit([
              editTitle(clip.id, next, fadeIn, fadeOut),
            ]);
          }}
        >
          <option value="lower_third">Lower third</option>
          <option value="centered">Centered</option>
          <option value="caption">Caption</option>
        </select>)}</Field>
      <Field label="Text">{(ids) => (<Input {...ids}
          value={text}
          maxLength={160}
          aria-label="Title text"
          onChange={(e) => {
            const t = e.target.value;
            setTemplate(
              template.template === "lower_third"
                ? { ...template, title: t }
                : { ...template, text: t },
            );
          }}
          onBlur={() =>
            void commit([
              editTitle(clip.id, template, fadeIn, fadeOut),
            ])
          }
        />)}</Field>
      {template.template === "lower_third" && (
        <Field label="Subtitle">{(ids) => (<Input {...ids}
            value={template.subtitle ?? ""}
            maxLength={160}
            aria-label="Title subtitle"
            onChange={(e) =>
              setTemplate({ ...template, subtitle: e.target.value || undefined })
            }
            onBlur={() =>
              void commit([
                editTitle(clip.id, template, fadeIn, fadeOut),
              ])
            }
          />)}</Field>
      )}
      <SecondsInput label="Horizontal position (%)" seconds={(model.corrections.positions[clip.id]?.x ?? 500) / 10}
        onCommit={(v) => void commit([positionTitle(clip.id, v * 10, model.corrections.positions[clip.id]?.y ?? 500)])} />
      <SecondsInput label="Vertical position (%)" seconds={(model.corrections.positions[clip.id]?.y ?? 500) / 10}
        onCommit={(v) => void commit([positionTitle(clip.id, model.corrections.positions[clip.id]?.x ?? 500, v * 10)])} />
      <div className="fe-inspect__row">
        <SecondsInput
          label="Start (s)"
          seconds={c.start_frame / model.fps}
          onCommit={(v) => void commit([trimTitle(clip.id, v * model.fps, c.frames)])}
        />
        <SecondsInput
          label="Duration (s)"
          seconds={c.frames / model.fps}
          onCommit={(v) => void commit([trimTitle(clip.id, c.start_frame, Math.max(1, v * model.fps))])}
        />
        <SecondsInput
          label="Fade in (s)"
          seconds={fadeIn / model.fps}
          onCommit={(v) =>
            void commit([editTitle(clip.id, template, Math.max(0, v * model.fps), fadeOut)])
          }
        />
        <SecondsInput
          label="Fade out (s)"
          seconds={fadeOut / model.fps}
          onCommit={(v) =>
            void commit([editTitle(clip.id, template, fadeIn, Math.max(0, v * model.fps))])
          }
        />
      </div>
    </div>
  );
}

function GatedRow({
  cap,
  caps,
  label,
}: {
  cap: Parameters<typeof unsupported>[0];
  caps: CapabilityReport | null;
  label: string;
}) {
  if (supports(cap, caps)) return null;
  const u = unsupported(cap);
  return (
    <div
      className="rk-card"
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        gap: "var(--s2, 8px)",
        padding: "var(--s2, 8px) var(--s3, 12px)",
        borderStyle: "dashed",
      }}
    >
      <span className="cr-muted" style={{ fontSize: 12 }}>{label}</span>
      <span className="cr-muted" style={{ fontSize: 10 }} title={u.reason}>
        Not supported by this engine
      </span>
    </div>
  );
}

function MuteControl({ clipId, model, commit }: { clipId: string; model: EditorModel; commit: (a: EditAction[]) => Promise<void> }) {
  return <Checkbox label="Mute clip audio"
    checked={model.corrections.audio[clipId]?.muted ?? false}
    onChange={(checked) => void commit([setMute(clipId, checked)])} />;
}
