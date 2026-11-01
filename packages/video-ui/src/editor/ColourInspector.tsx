/**
 * Colour section of the clip Inspector. Controls edit a local draft and call the
 * engine on pointer-up, key-up or blur; discrete edits (add, remove, reset,
 * select) commit at once. Scopes are the luma waveform and vectorscope at `atMs`.
 */
import { Button, Input, SegmentedControl, Select } from "@rightkit/react-shell/react";
import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";

import { assetUrl } from "../lib/asset.js";
import { errorMessage } from "../lib/engine.js";
import { HDR_UNAVAILABLE_NOTICE } from "../hooks/useHdrToSdr.js";
import {
  colourGet,
  colourScopes,
  colourSet,
  playbackRefreshFinishing,
  type ColourCurveChannel,
  type ColourGetResult,
  type ColourGrade,
  type ColourHsl,
  type ColourPt,
  type ColourRange,
  type ColourRgb,
  type ColourScopes,
  type ColourSetResult,
  type ColourWheel,
} from "../lib/media.js";

const MAX_POINTS = 16;
const MAX_HSL = 6;
/** Minimum x gap kept between neighbouring curve points. */
const EPS = 0.001;
const KEY_STEP = 0.01;

const NEUTRAL_GRADE: ColourGrade = {};
const NEUTRAL_WHEELS: Record<ColourWheel, ColourRgb> = { lift: [0, 0, 0], gamma: [1, 1, 1], gain: [1, 1, 1] };

type WheelDef = { key: ColourWheel; label: string; min: number; max: number };
const WHEELS: WheelDef[] = [
  { key: "lift", label: "Lift", min: -0.5, max: 0.5 },
  { key: "gamma", label: "Gamma", min: 0.2, max: 5 },
  { key: "gain", label: "Gain", min: 0, max: 4 },
];
const CHANNELS = [
  { key: "r", label: "R" },
  { key: "g", label: "G" },
  { key: "b", label: "B" },
] as const;
const CURVE_TABS: { value: ColourCurveChannel; label: string }[] = [
  { value: "master", label: "Master" },
  { value: "r", label: "R" },
  { value: "g", label: "G" },
  { value: "b", label: "B" },
];
const CURVE_NAMES: Record<ColourCurveChannel, string> = { master: "Master", r: "Red", g: "Green", b: "Blue" };
const HSL_RANGES: { value: ColourRange; label: string }[] = [
  { value: "reds", label: "Reds" },
  { value: "yellows", label: "Yellows" },
  { value: "greens", label: "Greens" },
  { value: "cyans", label: "Cyans" },
  { value: "blues", label: "Blues" },
  { value: "magentas", label: "Magentas" },
];
const SCOPE_TABS: { value: "graded" | "source"; label: string }[] = [
  { value: "graded", label: "Graded" },
  { value: "source", label: "Source" },
];

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);
const mean = (rgb: ColourRgb) => (rgb[0] + rgb[1] + rgb[2]) / 3;

function withChannel(rgb: ColourRgb, i: number, v: number): ColourRgb {
  return [i === 0 ? v : rgb[0], i === 1 ? v : rgb[1], i === 2 ? v : rgb[2]];
}

function shiftRgb(rgb: ColourRgb, d: number, lo: number, hi: number): ColourRgb {
  return [clamp(rgb[0] + d, lo, hi), clamp(rgb[1] + d, lo, hi), clamp(rgb[2] + d, lo, hi)];
}

/** The grade persisted for this clip (or the project); null when none is set. */
function storedOf(s: ColourGetResult, sourceId: string, toProject: boolean): ColourGrade | null {
  return toProject ? s.grade : (s.by_source[sourceId] ?? null);
}

function writeWheel(g: ColourGrade, k: ColourWheel, rgb: ColourRgb): ColourGrade {
  const wheels = { ...(g.wheels ?? NEUTRAL_WHEELS) };
  wheels[k] = rgb;
  return { ...g, wheels };
}

function curveOf(g: ColourGrade, ch: ColourCurveChannel): ColourPt[] {
  return g.curves?.[ch] ?? [];
}

function withCurve(g: ColourGrade, ch: ColourCurveChannel, pts: ColourPt[]): ColourGrade {
  const curves = { ...g.curves };
  if (pts.length === 0) delete curves[ch];
  else curves[ch] = pts;
  return { ...g, curves };
}

/** Allowed x range for point `i`, keeping it strictly between its neighbours. */
function curveBounds(pts: ColourPt[], i: number): [number, number] {
  const prev = pts[i - 1];
  const next = pts[i + 1];
  return [prev ? prev.x + EPS : 0, next ? next.x - EPS : 1];
}

/** Client coordinates to curve space: x 0..1 left to right, y 0..1 bottom to top. */
function curvePoint(box: Element, clientX: number, clientY: number): ColourPt {
  const r = box.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return { x: 0, y: 0 };
  return {
    x: clamp((clientX - r.left) / r.width, 0, 1),
    y: clamp(1 - (clientY - r.top) / r.height, 0, 1),
  };
}

/** Applies a successful `colour.set` reply to the persisted table. */
function applySaved(prev: ColourGetResult, sourceId: string, toProject: boolean, r: ColourSetResult): ColourGetResult {
  if (toProject) return { ...prev, grade: r.grade };
  const by_source = { ...prev.by_source };
  if (r.grade) by_source[sourceId] = r.grade;
  else delete by_source[sourceId];
  return { ...prev, by_source };
}

function SliderRow({
  label,
  name,
  testId,
  min,
  max,
  step,
  value,
  digits = 2,
  onInput,
  onCommit,
}: {
  /** Visible label. */
  label: string;
  /** Accessible name, unique within the panel. */
  name: string;
  testId?: string;
  min: number;
  max: number;
  step: number;
  value: number;
  digits?: number;
  onInput: (v: number) => void;
  onCommit: () => void;
}) {
  return (
    <label className="colour-row">
      <span className="colour-row__label">{label}</span>
      <Input
        type="range"
        className="colour-row__range"
        aria-label={name}
        data-testid={testId}
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onInput(Number(e.target.value))}
        onPointerUp={onCommit}
        onKeyUp={onCommit}
        onBlur={onCommit}
      />
      <output className="colour-row__value">{value.toFixed(digits)}</output>
    </label>
  );
}

export function ColourInspector({ dir, sourceId, atMs }: { dir: string; sourceId: string; atMs: number }) {
  const [state, setState] = useState<ColourGetResult | null>(null);
  const [project, setProject] = useState(false);
  const [draft, setDraft] = useState<ColourGrade>(NEUTRAL_GRADE);
  const [channel, setChannel] = useState<ColourCurveChannel>("master");
  const [graded, setGraded] = useState(true);
  const [scopes, setScopes] = useState<ColourScopes | null>(null);
  const [scopeTick, setScopeTick] = useState(0);
  const [actionError, setActionError] = useState<string | null>(null);
  const [scopeError, setScopeError] = useState<string | null>(null);
  /** Synchronous copy of `draft` so pointer and key handlers never read a stale grade. */
  const draftRef = useRef<ColourGrade>(NEUTRAL_GRADE);
  /** JSON of the grade persisted for the current edit target; a commit must differ from it. */
  const savedKey = useRef("null");
  /** Bumped per commit and per mode switch; only the newest reply updates draft bookkeeping. */
  const seqRef = useRef(0);
  /** Index of the curve point being dragged, if any. */
  const dragRef = useRef<number | null>(null);

  // Load the grade table when the clip's source or project changes.
  useEffect(() => {
    let cancelled = false;
    seqRef.current += 1;
    setState(null);
    setActionError(null);
    colourGet({ dir })
      .then((r) => {
        if (cancelled) return;
        const editable = storedOf(r, sourceId, false) ?? NEUTRAL_GRADE;
        draftRef.current = editable;
        setDraft(editable);
        savedKey.current = JSON.stringify(storedOf(r, sourceId, false));
        setProject(false);
        setState(r);
      })
      .catch((e: unknown) => {
        if (!cancelled) setActionError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [dir, sourceId]);

  // Scopes for the clip frame at `atMs`; debounced so trims and scrubs do not flood the engine.
  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      colourScopes({ dir, source: sourceId, at_ms: atMs, graded })
        .then((r) => {
          if (cancelled) return;
          setScopes(r);
          setScopeError(null);
        })
        .catch((e: unknown) => {
          if (!cancelled) setScopeError(errorMessage(e));
        });
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [dir, sourceId, atMs, graded, scopeTick]);

  const setGrade = (next: ColourGrade) => {
    draftRef.current = next;
    setDraft(next);
  };

  const send = (grade: ColourGrade | null, toProject: boolean) => {
    const key = JSON.stringify(grade);
    if (key === savedKey.current) return;
    const seq = ++seqRef.current;
    setActionError(null);
    colourSet({ dir, source: toProject ? undefined : sourceId, grade })
      .then((r) => {
        // Every reply is the persisted truth for its own target, so it always lands.
        setState((prev) => (prev ? applySaved(prev, sourceId, toProject, r) : prev));
        // The preview decodes through the saved grade; rebuild it so it shows this one.
        void playbackRefreshFinishing(dir).catch(() => undefined);
        if (seq !== seqRef.current) return;
        savedKey.current = JSON.stringify(r.grade);
        if (!toProject && r.grade === null) setGrade(NEUTRAL_GRADE);
        setScopeTick((t) => t + 1);
      })
      .catch((e: unknown) => {
        if (seq === seqRef.current) setActionError(errorMessage(e));
      });
  };

  const commitDraft = () => send(draftRef.current, project);

  const commitNow = (next: ColourGrade) => {
    setGrade(next);
    send(next, project);
  };

  const setMode = (toProject: boolean) => {
    if (!state || toProject === project) return;
    seqRef.current += 1;
    setActionError(null);
    setProject(toProject);
    setGrade(storedOf(state, sourceId, toProject) ?? NEUTRAL_GRADE);
    savedKey.current = JSON.stringify(storedOf(state, sourceId, toProject));
  };

  // Wheels: the master moves all three channels by the same delta.
  const wheelInput = (k: ColourWheel, i: number, v: number) => {
    const g = draftRef.current;
    setGrade(writeWheel(g, k, withChannel((g.wheels ?? NEUTRAL_WHEELS)[k], i, v)));
  };

  const wheelMasterInput = (def: WheelDef, v: number) => {
    const g = draftRef.current;
    const rgb = (g.wheels ?? NEUTRAL_WHEELS)[def.key];
    setGrade(writeWheel(g, def.key, shiftRgb(rgb, v - mean(rgb), def.min, def.max)));
  };

  const wheelReset = (k: ColourWheel) => commitNow(writeWheel(draftRef.current, k, NEUTRAL_WHEELS[k]));

  // Curves: click empty space to add, drag to move, double-click / Delete to remove.
  const onCurveDown = (e: PointerEvent<SVGSVGElement>) => {
    const g = draftRef.current;
    const pts = curveOf(g, channel);
    if (pts.length >= MAX_POINTS) return;
    const p = curvePoint(e.currentTarget, e.clientX, e.clientY);
    const next = [...pts, p].sort((a, b) => a.x - b.x);
    dragRef.current = next.indexOf(p);
    e.currentTarget.setPointerCapture(e.pointerId);
    setGrade(withCurve(g, channel, next));
  };

  const onCurveMove = (e: PointerEvent<SVGSVGElement>) => {
    const i = dragRef.current;
    if (i === null) return;
    const g = draftRef.current;
    const pts = curveOf(g, channel);
    const p = curvePoint(e.currentTarget, e.clientX, e.clientY);
    const [lo, hi] = curveBounds(pts, i);
    const moved = pts.map((q, j) => (j === i ? { x: clamp(p.x, lo, hi), y: p.y } : q));
    setGrade(withCurve(g, channel, moved));
  };

  const onCurveUp = () => {
    if (dragRef.current === null) return;
    dragRef.current = null;
    commitDraft();
  };

  const removePoint = (i: number) => {
    const g = draftRef.current;
    commitNow(withCurve(g, channel, curveOf(g, channel).filter((_, j) => j !== i)));
  };

  const addPoint = () => {
    const g = draftRef.current;
    const pts = curveOf(g, channel);
    if (pts.length >= MAX_POINTS) return;
    // Place the point on the identity diagonal at the middle of the widest gap.
    const xs = [0, ...pts.map((p) => p.x), 1];
    let x = 0.5;
    let widest = -1;
    xs.forEach((a, k) => {
      const b = xs[k + 1];
      if (b === undefined || b - a <= widest) return;
      widest = b - a;
      x = (a + b) / 2;
    });
    commitNow(withCurve(g, channel, [...pts, { x, y: x }].sort((a, b) => a.x - b.x)));
  };

  const onPointKey = (i: number, e: KeyboardEvent<SVGCircleElement>) => {
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      removePoint(i);
      return;
    }
    const dx = e.key === "ArrowLeft" ? -KEY_STEP : e.key === "ArrowRight" ? KEY_STEP : 0;
    const dy = e.key === "ArrowDown" ? -KEY_STEP : e.key === "ArrowUp" ? KEY_STEP : 0;
    if (dx === 0 && dy === 0) return;
    e.preventDefault();
    const g = draftRef.current;
    const pts = curveOf(g, channel);
    const at = pts[i];
    if (!at) return;
    const [lo, hi] = curveBounds(pts, i);
    const moved = pts.map((p, j) =>
      j === i ? { x: clamp(at.x + dx, lo, hi), y: clamp(at.y + dy, 0, 1) } : p,
    );
    setGrade(withCurve(g, channel, moved));
  };

  // HSL secondaries.
  const patchHsl = (i: number, patch: Partial<ColourHsl>, commit: boolean) => {
    const g = draftRef.current;
    const list = (g.hsl ?? []).map((h, j) => (j === i ? { ...h, ...patch } : h));
    if (commit) commitNow({ ...g, hsl: list });
    else setGrade({ ...g, hsl: list });
  };

  const addHsl = () => {
    const g = draftRef.current;
    const list = g.hsl ?? [];
    if (list.length >= MAX_HSL) return;
    const used = new Set(list.map((h) => h.range));
    const range = HSL_RANGES.find((r) => !used.has(r.value))?.value ?? "reds";
    commitNow({ ...g, hsl: [...list, { range, hue_shift: 0, saturation: 0, lightness: 0, strength: 1 }] });
  };

  const removeHsl = (i: number) => {
    const g = draftRef.current;
    commitNow({ ...g, hsl: (g.hsl ?? []).filter((_, j) => j !== i) });
  };

  const stored = state ? storedOf(state, sourceId, false) : null;
  const hasOwn = stored !== null;
  const error = actionError ?? scopeError;
  const wheels = draft.wheels ?? NEUTRAL_WHEELS;
  const sat = draft.saturation ?? 1;
  const hsl = draft.hsl ?? [];
  const points = curveOf(draft, channel);
  const curvePolyline = points.map((p) => `${p.x * 100},${(1 - p.y) * 100}`).join(" ");

  return (
    <section className="colour-section" aria-label="Colour" data-testid="colour-section">
      <h3 className="colour-section__title">Colour</h3>

      {state?.capabilities?.hdr_to_sdr === false && (
        <p className="cr-muted colour-status" data-testid="colour-hdr-notice">
          {HDR_UNAVAILABLE_NOTICE}
        </p>
      )}
      {state === null && actionError === null && <p className="cr-muted colour-status">Loading colour…</p>}

      {state !== null && (
        <>
          <p className="cr-muted colour-source">
            {project
              ? "Editing the project grade."
              : hasOwn
                ? "This clip uses its own grade."
                : "This clip uses the project grade."}
          </p>
          <div className="colour-actions">
            {hasOwn && !project && (
              <Button size="sm" variant="secondary" data-testid="colour-use-project" onClick={() => send(null, false)}>
                Use project grade
              </Button>
            )}
            <Button size="sm" variant="ghost" aria-pressed={project} onClick={() => setMode(!project)}>
              Edit project grade
            </Button>
          </div>

          <div className="colour-group">
            <h4 className="colour-group__title">Wheels</h4>
            {WHEELS.map((def) => {
              const rgb = wheels[def.key];
              return (
                <div key={def.key} className="colour-wheel">
                  <div className="colour-wheel__head">
                    <span className="colour-wheel__name">{def.label}</span>
                    <Button size="sm" variant="ghost" onClick={() => wheelReset(def.key)}>
                      Reset
                    </Button>
                  </div>
                  <SliderRow
                    label="Master"
                    name={`${def.label} master`}
                    testId={`colour-${def.key}-master`}
                    min={def.min}
                    max={def.max}
                    step={0.01}
                    value={mean(rgb)}
                    onInput={(v) => wheelMasterInput(def, v)}
                    onCommit={commitDraft}
                  />
                  {CHANNELS.map((c, i) => (
                    <SliderRow
                      key={c.key}
                      label={c.label}
                      name={`${def.label} ${c.label}`}
                      testId={`colour-${def.key}-${c.key}`}
                      min={def.min}
                      max={def.max}
                      step={0.01}
                      value={rgb[i]}
                      onInput={(v) => wheelInput(def.key, i, v)}
                      onCommit={commitDraft}
                    />
                  ))}
                </div>
              );
            })}
          </div>

          <div className="colour-group">
            <h4 className="colour-group__title">Curves</h4>
            <SegmentedControl<ColourCurveChannel>
              label="Curve channel"
              value={channel}
              options={CURVE_TABS}
              onChange={setChannel}
            />
            <svg
              className="colour-curve"
              viewBox="0 0 100 100"
              role="group"
              aria-label={`${CURVE_NAMES[channel]} curve`}
              data-testid="colour-curve"
              onPointerDown={onCurveDown}
              onPointerMove={onCurveMove}
              onPointerUp={onCurveUp}
              onPointerCancel={onCurveUp}
            >
              <rect className="colour-curve__hit" x="0" y="0" width="100" height="100" fill="transparent" />
              <line className="colour-curve__identity" x1="0" y1="100" x2="100" y2="0" />
              {points.length > 0 && <polyline className="colour-curve__line" fill="none" points={curvePolyline} />}
              {points.map((p, i) => (
                <circle
                  key={i}
                  className="colour-curve__point"
                  cx={p.x * 100}
                  cy={(1 - p.y) * 100}
                  r={2.5}
                  tabIndex={0}
                  role="slider"
                  aria-label={`${CURVE_NAMES[channel]} point ${i + 1} of ${points.length}`}
                  aria-valuetext={`input ${p.x.toFixed(2)}, output ${p.y.toFixed(2)}`}
                  onPointerDown={(e) => {
                    e.stopPropagation();
                    dragRef.current = i;
                    e.currentTarget.setPointerCapture(e.pointerId);
                  }}
                  onKeyDown={(e) => onPointKey(i, e)}
                  onKeyUp={commitDraft}
                  onBlur={commitDraft}
                  onDoubleClick={() => removePoint(i)}
                />
              ))}
            </svg>
            <div className="colour-actions">
              <Button size="sm" variant="secondary" disabled={points.length >= MAX_POINTS} onClick={addPoint}>
                Add point
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={points.length === 0}
                onClick={() => commitNow(withCurve(draftRef.current, channel, []))}
              >
                Reset curve
              </Button>
            </div>
          </div>

          <div className="colour-group">
            <div className="colour-group__head">
              <h4 className="colour-group__title">HSL secondary</h4>
              <Button size="sm" variant="secondary" disabled={hsl.length >= MAX_HSL} onClick={addHsl}>
                Add secondary
              </Button>
            </div>
            {hsl.map((h, i) => (
              <div key={i} className="colour-hsl">
                <div className="colour-hsl__head">
                  <Select
                    label={`Secondary ${i + 1} range`}
                    value={h.range}
                    options={HSL_RANGES}
                    onChange={(value) => {
                      const r = HSL_RANGES.find((x) => x.value === value);
                      if (r) patchHsl(i, { range: r.value }, true);
                    }}
                  />
                  <Button size="sm" variant="ghost" aria-label={`Remove secondary ${i + 1}`} onClick={() => removeHsl(i)}>
                    Remove
                  </Button>
                </div>
                <SliderRow
                  label="Hue shift"
                  name={`Secondary ${i + 1} hue shift`}
                  min={-180}
                  max={180}
                  step={1}
                  value={h.hue_shift}
                  digits={0}
                  onInput={(v) => patchHsl(i, { hue_shift: v }, false)}
                  onCommit={commitDraft}
                />
                <SliderRow
                  label="Saturation"
                  name={`Secondary ${i + 1} saturation`}
                  min={-1}
                  max={1}
                  step={0.01}
                  value={h.saturation}
                  onInput={(v) => patchHsl(i, { saturation: v }, false)}
                  onCommit={commitDraft}
                />
                <SliderRow
                  label="Lightness"
                  name={`Secondary ${i + 1} lightness`}
                  min={-1}
                  max={1}
                  step={0.01}
                  value={h.lightness}
                  onInput={(v) => patchHsl(i, { lightness: v }, false)}
                  onCommit={commitDraft}
                />
                <SliderRow
                  label="Strength"
                  name={`Secondary ${i + 1} strength`}
                  min={0}
                  max={1}
                  step={0.01}
                  value={h.strength}
                  onInput={(v) => patchHsl(i, { strength: v }, false)}
                  onCommit={commitDraft}
                />
              </div>
            ))}
          </div>

          <div className="colour-group">
            <h4 className="colour-group__title">Saturation</h4>
            <SliderRow
              label="Saturation"
              name="Saturation"
              testId="colour-saturation"
              min={0}
              max={2}
              step={0.01}
              value={sat}
              onInput={(v) => setGrade({ ...draftRef.current, saturation: v })}
              onCommit={commitDraft}
            />
          </div>
        </>
      )}

      <div className="colour-group">
        <div className="colour-group__head">
          <h4 className="colour-group__title">Scopes</h4>
          <SegmentedControl
            label="Scope grade"
            value={graded ? "graded" : "source"}
            options={SCOPE_TABS}
            onChange={(v) => setGraded(v === "graded")}
          />
        </div>
        <div className="colour-scope__box colour-scope__box--wide">
          {scopes ? (
            <img
              className="colour-scope__img"
              src={assetUrl(scopes.waveform)}
              alt="Luma waveform"
              data-testid="colour-scope-waveform"
            />
          ) : (
            <span className="cr-muted colour-status">Loading scopes…</span>
          )}
        </div>
        <div className="colour-scope__box colour-scope__box--square">
          {scopes ? (
            <img
              className="colour-scope__img"
              src={assetUrl(scopes.vectorscope)}
              alt="Vectorscope"
              data-testid="colour-scope-vectorscope"
            />
          ) : (
            <span className="cr-muted colour-status">Loading scopes…</span>
          )}
        </div>
        {scopes && (
          <dl className="colour-stats">
            <div>
              <dt>Y min</dt>
              <dd>{scopes.stats.y_min.toFixed(2)}</dd>
            </div>
            <div>
              <dt>Y max</dt>
              <dd>{scopes.stats.y_max.toFixed(2)}</dd>
            </div>
            <div>
              <dt>Y avg</dt>
              <dd>{scopes.stats.y_avg.toFixed(2)}</dd>
            </div>
            <div>
              <dt>Sat max</dt>
              <dd>{scopes.stats.sat_max.toFixed(2)}</dd>
            </div>
          </dl>
        )}
        {scopes && (scopes.clipped.high || scopes.clipped.low) && (
          <div className="colour-clipping" data-testid="colour-clipping" role="status">
            {scopes.clipped.high && <span>Clipping: highlights</span>}
            {scopes.clipped.low && <span>Clipping: shadows</span>}
          </div>
        )}
      </div>

      {error !== null && (
        <p role="alert" className="colour-error" data-testid="colour-error">
          {error}
        </p>
      )}
    </section>
  );
}
