// Shorts — docs/design/app-ui.md §3.6.
import { Badge, Button, Checkbox, EmptyState, Field, PageShell, useToast } from "@rightkit/react-shell/react";
import { framingLabel } from "../../lib/media.js";
import { assetUrl } from "../../lib/asset.js";
import { useVideoShorts, useShortThumbnail, useShortTrimDrag, SHORT_ASPECT, type ShortProposal } from "../hooks/useVideoShorts.js";
import { FramingDrawer } from "./AutoEditView.js";
import { RangePreview } from "../../components/RangePreview.js";
import { Drawer, ErrorPanel, ProgressRow, Thumbnail, timecode, Icon } from "../ui/index.js";
import { hostAppName } from "../appName.js";

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
  const { track, drag } = useShortTrimDrag(proposal, startMs, endMs, onChange);
  const lo = Math.max(0, proposal.start_ms - 5000);
  const hi = proposal.end_ms + 5000;
  const span = hi - lo;
  const pct = (ms: number) => `${(((ms - lo) / span) * 100).toFixed(2)}%`;


  return (
    <div className="shorts-trim">
      <span className="shorts-trim__time cr-mono">{timecode(lo)}</span>
      <div className="shorts-trim__track" ref={track}>
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
  const src = useShortThumbnail(dir, source, atMs);
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

export function ShortsView({ dir, onChanged, refreshChangedProject }: { dir: string; onChanged: () => Promise<void>; refreshChangedProject?: (dir: string) => Promise<void> }) {
  const toast = useToast();
  const { sources, source, setSource, busy, busyOp, error, shorts, selected, setSelected, open, setOpen, trim, setTrim, searched, plan, framingChanged, autoFraming, framing, setFraming, selectedSource, framingNeeded, framingAllowed, propose, exportShort, exportSelected, addToTimeline, openDrawer } = useVideoShorts({
    dir, onChanged, refreshChangedProject,
    onExported: (r) => toast(`Short ${r.variant} exported`, { kind: "success" }),
    onAdded: (p) => toast(`Added “${p.title}” to the timeline`, { kind: "success" }),
  });
  const framingBlock = framingAllowed ? undefined : "Approve this source's 9:16 framing first";

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
            disabled={(!selectedSource?.has_transcript) || (busy !== null)}
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
                disabled={busy !== null}
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
                    (ok) => ok && setOpen((current) => current === open ? null : current),
                  )
                }
                title={framingBlock ?? "Renders the trimmed range (bounds snap to word edges)"}
                disabled={(!framingAllowed) || (busy !== null)}
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
                key={open.id}
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
          onChanged={framingChanged}
        />
      )}
    </PageShell>
  );
}
