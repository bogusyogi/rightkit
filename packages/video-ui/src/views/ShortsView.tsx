// Shorts — docs/design/app-ui.md §3.6.
import { Badge, Button, Checkbox, EmptyState, Field, PageShell, useToast } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../lib/engine.js";
import {
  type ShortProposal,
  type ShortsResult,
  type ReframeSummary,
  framingApproval,
  framingLabel,
  settingsGet,
  reframeShow,
  shortsAccept,
  shortsPropose,
  sourceList,
  thumbnails,
} from "../lib/media.js";
import { assetUrl } from "../lib/asset.js";
import { appendClip, projectShow, type ProjectShow } from "../lib/project.js";
import { FramingDrawer } from "./AutoEditView.js";
import { RangePreview } from "../components/RangePreview.js";
import { Drawer, ErrorPanel, ProgressRow, Thumbnail, timecode, Icon } from "../ui/index.js";
import { hostAppName } from "../appName.js";

type SourceRow = { id: string; name: string; path: string; exists: boolean; has_transcript: boolean };

/** Shorts render vertical: the engine requires an approved plan for this aspect. */
const SHORT_ASPECT = "9:16";

/** Mirrors the engine's rule: a source within 5% of 9:16 renders full-frame without a plan. */
function needsFraming(show: ProjectShow | null, source: string): boolean {
  const v = show?.timeline.sources.find((t) => t.id === source)?.video;
  if (!v) return false;
  return Math.abs(v.width / v.height / (9 / 16) - 1) >= 0.05;
}

/** Two-handle boundary trim over a ±5s context window around the proposal. */
function BoundaryTrim({
  proposal,
  startMs,
  endMs,
  onChange,
}: {
  proposal: ShortProposal;
  startMs: number;
  endMs: number;
  onChange: (start: number, end: number) => void;
}) {
  const track = useRef<HTMLDivElement | null>(null);
  const lo = Math.max(0, proposal.start_ms - 5000);
  const hi = proposal.end_ms + 5000;
  const span = hi - lo;
  const pct = (ms: number) => `${(((ms - lo) / span) * 100).toFixed(2)}%`;

  const drag = (which: "in" | "out") => (e: React.PointerEvent) => {
    e.preventDefault();
    const el = track.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      const frac = Math.max(0, Math.min(1, (ev.clientX - rect.left) / rect.width));
      const ms = lo + frac * span;
      if (which === "in") onChange(Math.min(ms, endMs - 250), endMs);
      else onChange(startMs, Math.max(ms, startMs + 250));
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  return (
    <div className="shorts-trim" ref={track}>
      <span className="shorts-trim__time cr-mono">{timecode(lo)}</span>
      <div className="shorts-trim__track">
        <div className="shorts-trim__sel" style={{ left: pct(startMs), right: `calc(100% - ${pct(endMs)})` }} />
        <button
          type="button"
          className="shorts-trim__handle"
          style={{ left: pct(startMs) }}
          aria-label="Trim start"
          onPointerDown={drag("in")}
        />
        <button
          type="button"
          className="shorts-trim__handle"
          style={{ left: pct(endMs) }}
          aria-label="Trim end"
          onPointerDown={drag("out")}
        />
      </div>
      <span className="shorts-trim__time cr-mono">{timecode(hi)}</span>
    </div>
  );
}

function ShortThumb({ dir, source, atMs, videoPath }: { dir: string; source: string; atMs: number; videoPath?: string }) {
  const [src, setSrc] = useState<string | undefined>();
  useEffect(() => {
    let cancelled = false;
    setSrc(undefined);
    thumbnails(dir, { source }, { at_ms: atMs, width: 360 })
      .then((r) => !cancelled && setSrc(r.frames[0] ? assetUrl(r.frames[0].path) : undefined))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [dir, source, atMs]);
  return <Thumbnail src={src} aspect="9 / 16" video={videoPath ? `${assetUrl(videoPath)}#t=${atMs / 1000}` : undefined} />;
}

/** Engine platform ids → spec labels (R6): Shorts / Reels / TikTok. */
function platformLabel(id: string): string {
  const s = id.toLowerCase();
  if (s.includes("short")) return "Shorts";
  if (s.includes("reel")) return "Reels";
  if (s.includes("tiktok")) return "TikTok";
  return id.charAt(0).toUpperCase() + id.slice(1);
}

export function ShortsView({ dir, onChanged }: { dir: string; onChanged: () => Promise<void> }) {
  const toast = useToast();
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [source, setSource] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  // Which operation `busy` (a short id) stands for, so each button shows its own busy text.
  const [busyOp, setBusyOp] = useState<"add" | "export" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [shorts, setShorts] = useState<ShortsResult | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [open, setOpen] = useState<ShortProposal | null>(null);
  const [trim, setTrim] = useState<{ start: number; end: number } | null>(null);
  const [searched, setSearched] = useState(false);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [plan, setPlan] = useState<ReframeSummary | "none" | undefined>(undefined);
  const [autoFraming, setAutoFraming] = useState(false);
  const [framing, setFraming] = useState(false);

  const refresh = useCallback(async () => {
    const [s, p, settings] = await Promise.all([sourceList(dir), projectShow(dir), settingsGet(dir)]);
    setAutoFraming(framingApproval(settings) === "auto");
    const rows = s.sources as SourceRow[];
    setSources(rows);
    setShow(p);
    setSource((cur) => cur || rows.find((x) => x.has_transcript)?.id || rows[0]?.id || "");
  }, [dir]);

  useEffect(() => {
    refresh().catch((e) => setError(errorMessage(e)));
  }, [refresh]);

  const selectedSource = sources.find((s) => s.id === source);
  const framingNeeded = needsFraming(show, source);

  // The vertical plan for the chosen source; "none" until one is analysed.
  useEffect(() => {
    setPlan(undefined);
    if (!source || !framingNeeded) return;
    let live = true;
    reframeShow(dir, source, SHORT_ASPECT)
      .then((r) => live && setPlan(r))
      .catch(() => live && setPlan("none"));
    return () => {
      live = false;
    };
  }, [dir, source, framingNeeded]);

  const framingApproved = !framingNeeded || (plan !== undefined && plan !== "none" && plan.approved);
  const framingAllowed = autoFraming || framingApproved;
  const framingBlock = framingAllowed ? undefined : "Approve this source's 9:16 framing first";

  const propose = async () => {
    setBusy("propose");
    setError(null);
    try {
      const r = await shortsPropose(dir, source, 6);
      setShorts(r);
      setSelected(new Set());
      setSearched(true);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const exportShort = async (p: ShortProposal, bounds?: { start: number; end: number }) => {
    setBusy(p.id);
    setBusyOp("export");
    setError(null);
    try {
      const trimmed =
        bounds && (bounds.start !== p.start_ms || bounds.end !== p.end_ms)
          ? { start_ms: bounds.start, end_ms: bounds.end }
          : undefined;
      const r = await shortsAccept(dir, source, p.id, trimmed);
      toast(`Short ${r.variant} exported`, { kind: "success" });
      if (framingNeeded) setPlan(await reframeShow(dir, source, SHORT_ASPECT));
      await refresh();
      await onChanged();
      return true;
    } catch (e) {
      setError(errorMessage(e));
      return false;
    } finally {
      setBusy(null);
      setBusyOp(null);
    }
  };

  const exportSelected = async () => {
    const list = (shorts?.proposals ?? []).filter((p) => selected.has(p.id));
    for (const p of list) {
      // eslint-disable-next-line no-await-in-loop -- sequential: one render worker
      if (!(await exportShort(p))) break;
    }
  };

  const addToTimeline = async (p: ShortProposal, startMs: number, endMs: number) => {
    setBusy(p.id);
    setBusyOp("add");
    setError(null);
    try {
      await appendClip(dir, source, startMs, endMs);
      toast(`Added “${p.title}” to the timeline`, { kind: "success" });
      await onChanged();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
      setBusyOp(null);
    }
  };

  const openDrawer = (p: ShortProposal) => {
    setOpen(p);
    setTrim({ start: p.start_ms, end: p.end_ms });
  };

  return (
    <PageShell title="Shorts" actions={
        <>
          <Field label="Source">{(ids) => (<select {...ids} className="rk-input cr-select" value={source} onChange={(e) => setSource(e.target.value)}>
              {sources.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                  {s.has_transcript ? "" : " (not transcribed)"}
                </option>
              ))}
            </select>)}</Field>
          <Button
            variant="primary"
            title={selectedSource?.has_transcript ? undefined : "Transcribe this source first"}
            onClick={() => void propose()}
            disabled={(!selectedSource?.has_transcript) || (busy === "propose")}
            aria-busy={(busy === "propose") || undefined}
          >{!(busy === "propose") && <Icon name="scissors" />}
            {busy === "propose" ? "Finding…" : "Find shorts"}
          </Button>
        </>
      } maxWidth={0} subtitle={sources.length ? `${sources.length} source${sources.length === 1 ? "" : "s"}` : undefined}>
      <div className="shorts-body">
        {busy === "propose" && <ProgressRow label="Ranking clip candidates" status="Indexing" fraction={undefined} />}
        {error && <ErrorPanel title="Shorts failed" detail={error} />}
        {framingNeeded && (autoFraming || plan !== undefined) && (
          <div className="shorts-framing">
            <Badge tone={framingAllowed ? "ok" : "warn"}>{framingLabel(plan && plan !== "none" ? plan : undefined, autoFraming)}</Badge>
            <span className="cr-muted">{autoFraming ? "Shorts render 9:16; saved policy approves tracked framing during export." : "Shorts render 9:16; approve how this source is cropped before exporting."}</span>
            <Button variant="secondary" onClick={() => setFraming(true)}>
              Review framing
            </Button>
          </div>
        )}

        {searched && shorts && shorts.proposals.length === 0 && (
          <EmptyState title="No shorts found" icon={<Icon name="scissors" width={28} height={28} />}>
            No strong clip candidates in this source. Try another clip.
          </EmptyState>
        )}
        {!searched && !busy && !error && (
          <EmptyState title="Find the best moments" icon={<Icon name="scissors" width={28} height={28} />}>
            Pick a transcribed source and {hostAppName()} ranks clip candidates for vertical platforms.
          </EmptyState>
        )}

        {shorts && shorts.proposals.length > 0 && (
          <>
            {shorts.ranking && (
              <div className="shorts-ranking">
                {shorts.ranking.method === "llm" ? (
                  <Badge tone="accent">Ranked by {shorts.ranking.model ?? "LLM"}</Badge>
                ) : (
                  <span title={shorts.ranking.fallback_reason ?? undefined}><Badge tone="warn">
                    Heuristic ranking
                  </Badge></span>
                )}
                {shorts.ranking.method !== "llm" && shorts.ranking.fallback_reason && (
                  <span className="cr-muted">Why: {shorts.ranking.fallback_reason}</span>
                )}
                {shorts.ranking.prompt_budget &&
                  shorts.ranking.prompt_budget.sentences_sent <
                    shorts.ranking.prompt_budget.sentences_offered && (
                    <span className="cr-muted">
                      Prompt budget sent {shorts.ranking.prompt_budget.sentences_sent} of{" "}
                      {shorts.ranking.prompt_budget.sentences_offered} sentences
                    </span>
                  )}
              </div>
            )}
            <div className="shorts-grid">
              {shorts.proposals.map((p) => (
                <div key={p.id} className={`shorts-card ${selected.has(p.id) ? "shorts-card--sel" : ""}`}>
                  <button type="button" className="shorts-card__thumb" onClick={() => openDrawer(p)}>
                    <ShortThumb
                      dir={dir}
                      source={source}
                      atMs={p.start_ms}
                      videoPath={selectedSource?.path}
                    />
                    <span className="shorts-card__rank cr-mono">#{p.rank}</span>
                  </button>
                  <div className="shorts-card__meta">
                    <span className="cr-mono cr-muted">{timecode(p.duration_ms)}</span>
                    {p.scores?.composite != null && (
                      <Badge tone="accent">Score {Math.round(p.scores.composite * 100)}</Badge>
                    )}
                  </div>
                  {(p.platform_fit ?? []).some((f) => f.fits) && (
                    <div className="shorts-card__platforms">
                      {(p.platform_fit ?? [])
                        .filter((f) => f.fits)
                        .map((f) => (
                          <Badge key={f.platform}>{platformLabel(f.platform)}</Badge>
                        ))}
                    </div>
                  )}
                  <div className="shorts-card__hook cr-clamp-2">{p.title}</div>
                  <Checkbox
                    className="shorts-card__pick"
                    label="Select"
                    checked={selected.has(p.id)}
                    onChange={(checked) =>
                      setSelected((s) => {
                        const n = new Set(s);
                        if (checked) n.add(p.id);
                        else n.delete(p.id);
                        return n;
                      })
                    }
                  />
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      {selected.size > 0 && (
        <div className="shorts-bar">
          <span>{selected.size} selected</span>
          <Button
            variant="primary"
            title={framingBlock}
            onClick={() => void exportSelected()}
            disabled={(!framingAllowed) || (busy !== null)}
            aria-busy={(busy !== null) || undefined}
          >{!(busy !== null) && <Icon name="download" />}
            {busyOp === "export" ? "Exporting…" : "Export shorts"}
          </Button>
        </div>
      )}

      <Drawer
        title={open?.title ?? "Short"}
        open={open !== null && !framing}
        onClose={() => setOpen(null)}
        footer={
          open && (
            <>
              <Button
                variant="secondary"
                onClick={() => void addToTimeline(open, trim?.start ?? open.start_ms, trim?.end ?? open.end_ms)}
                disabled={(busy === open.id)}
                aria-busy={(busy === open.id) || undefined}
              >
                {busyOp === "add" && busy === open.id ? "Adding…" : "Add to timeline"}
              </Button>
              {!framingAllowed && (
                <Button variant="secondary" onClick={() => setFraming(true)}>
                  Review framing
                </Button>
              )}
              <Button
                variant="primary"
                onClick={() =>
                  void exportShort(open, trim ? { start: trim.start, end: trim.end } : undefined).then(
                    (ok) => ok && setOpen(null),
                  )
                }
                title={framingBlock ?? "Renders the trimmed range (bounds snap to word edges)"}
                disabled={(!framingAllowed) || (busy === open.id)}
                aria-busy={(busy === open.id) || undefined}
              >
                {busyOp === "export" && busy === open.id ? "Exporting…" : "Export"}
              </Button>
            </>
          )
        }
      >
        {open && selectedSource && (
          <>
            <RangePreview
              loop
              path={selectedSource.path}
              startMs={trim?.start ?? open.start_ms}
              endMs={trim?.end ?? open.end_ms}
            />
            <Field label="Boundaries" note={`${timecode(trim?.start ?? open.start_ms)} – ${timecode(trim?.end ?? open.end_ms)}`}>{() => (<BoundaryTrim
                proposal={open}
                startMs={trim?.start ?? open.start_ms}
                endMs={trim?.end ?? open.end_ms}
                onChange={(start, end) => setTrim({ start, end })}
              />)}</Field>
            <p className="cr-muted shorts-drawer__rationale">{open.rationale}</p>
            {error && (
              <p role="alert" className="shorts-drawer__error">
                {error}
              </p>
            )}
          </>
        )}
      </Drawer>
      {framing && selectedSource && (
        <FramingDrawer
          dir={dir}
          source={selectedSource}
          aspect={SHORT_ASPECT}
          plan={plan}
          autoFraming={autoFraming}
          onClose={() => setFraming(false)}
          onChanged={setPlan}
        />
      )}
    </PageShell>
  );
}
