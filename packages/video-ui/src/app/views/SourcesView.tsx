import { MediaPlayer } from "@rightkit/media-ui";
import { Badge, Button, Card, EmptyState, PageShell } from "@rightkit/react-shell/react";
import { useCallback } from "react";
import { baseName } from "../../lib/project.js";
import { Drawer, Icon, ProgressRow, Thumbnail, fileSize, timecode } from "../ui/index.js";
import { useVideoSources, type VideoSourceIntent, type VideoSourceRow, type VideoMicState } from "../hooks/useVideoSources.js";

const INTENT_LABEL: Record<VideoSourceIntent, string> = {
  "talking-head": "Talking head",
  mixed: "Mixed",
  visual: "Visual",
};

export function SourcesView({
  dir,
  onChanged,
  refreshChangedProject,
  onNavigate,
}: {
  dir: string;
  onChanged: () => Promise<void>;
  refreshChangedProject?: (dir: string) => Promise<void>;
  onNavigate?: (to: "recipe" | "finished") => void;
}) {
  const refreshProject = useCallback((changedDir: string) => refreshChangedProject?.(changedDir) ?? onChanged(), [onChanged, refreshChangedProject]);
  const { cards, drawer, addProgress, importing, speechReady, error, relinkRejected, dropActive, totalMs, canMakeEdit, selectSource, stopCardAction, importMedia, relink, cycleIntent, alignMic, runTranscribe, copyHash, reveal } = useVideoSources({ dir, onChanged: refreshProject });
  const notice = error ?? (relinkRejected === "mismatch" ? "The selected file does not match this source." : relinkRejected === "unverified" ? "The replacement file could not be verified." : null);
  return (
    <PageShell title="Media" actions={
        <>
          <Button variant="secondary" onClick={() => void importMedia()}><Icon name="upload" />
            Import…
          </Button>
          <Button
            variant="primary"
            disabled={!canMakeEdit}
            title={!canMakeEdit ? "Import at least one source first" : undefined}
            onClick={() => onNavigate?.("recipe")}
          ><Icon name="wand-sparkles" />
            Make finished edit
          </Button>
        </>
      } maxWidth={0} subtitle={cards.length ? `${cards.length} clips · ${timecode(totalMs)} total` : undefined}>
      {dropActive && (
        <div className="media-drop" aria-hidden>
          <span>Drop to add</span>
        </div>
      )}
      {notice && (
        <p role="alert" style={{ margin: 0, color: "var(--rk-bad)", fontSize: 13 }}>
          {notice}
        </p>
      )}
      {importing > 0 && <ProgressRow label="Importing" status="working" fraction={undefined} />}
      {cards.length === 0 && importing === 0 ? (
        <EmptyState
          title="No media yet"
          action={
            <Button variant="primary" onClick={() => void importMedia()}><Icon name="upload" />
              Import…
            </Button>
          }
          icon={<Icon name="film" width={28} height={28} />}
        >
          Drop clips here or import them.
        </EmptyState>
      ) : (
        <div className="media-grid">
          {cards.map(({ row: r, busy, missing, changed, dimensions, fps, durationMs, intent, hasAudio, preview, thumbnail, transcriptStatus }) => {
            const dims = dimensions ? `${dimensions.width}×${dimensions.height}` : null;
            return (
              <Card key={r.id} onClick={() => selectSource(r.id)} label={r.name || baseName(r.path)} className="media-card">
                <Thumbnail
                  src={thumbnail}
                  badge={durationMs ? timecode(durationMs) : undefined}
                  video={preview}
                  corner={
                    missing || changed ? (
                      <span className="media-card__missing">
                        <Icon name="triangle-alert" width={28} height={28} />
                      </span>
                    ) : undefined
                  }
                />
                <span className="media-card__name" title={r.path}>
                  {r.name || baseName(r.path)}
                </span>
                <span className="media-card__meta">
                  {[dims, fps ? `${Math.round(fps)} fps` : null, hasAudio === false ? "No audio" : "Audio"]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
                {busy ? (
                  <ProgressRow
                    label={busy === "hashing" ? "Hashing" : transcriptStatus?.message ?? "Transcribing"}
                    status="working"
                    fraction={busy === "transcribing" ? transcriptStatus?.fraction : undefined}
                  />
                ) : (
                  // Chips act on their own; they must not also open the card's drawer.
                  <span
                    className="media-card__chips"
                    onClick={stopCardAction}
                    onKeyDown={stopCardAction}
                  >
                    {missing ? (
                      <Badge tone="warn"><Icon name="triangle-alert" width={12} height={12} />
                        Missing
                      </Badge>
                    ) : changed ? (
                      <Badge tone="warn"><Icon name="triangle-alert" width={12} height={12} />
                        Changed on disk
                      </Badge>
                    ) : (
                      <>
                        <Button variant="ghost" size="sm" className="cr-chip-btn" onClick={() => cycleIntent(r)} title={"Detected type — click to change"}><Badge
                          tone={intent === "talking-head" ? "ok" : intent === "mixed" ? "accent" : "neutral"}
                        >
                          {INTENT_LABEL[intent]}
                        </Badge></Button>
                        <TranscriptChip row={r} speechReady={speechReady} />
                      </>
                    )}
                    {(missing || changed) && (
                      <Button variant="ghost" size="sm" className="cr-chip-btn" onClick={() => void relink(r)} title={"Find the file again"}><Badge><Icon name="link" width={12} height={12} />
                        Relink…
                      </Badge></Button>
                    )}
                  </span>
                )}
              </Card>
            );
          })}
          {/* Files mid-import aren't listed yet — show their real progress (§3.2). */}
          {Object.entries(addProgress).map(([file, p]) => (
            <Card key={`add-${file}`} className="media-card" label={baseName(file)}>
              <Thumbnail />
              <span className="media-card__name" title={file}>
                {baseName(file)}
              </span>
              <ProgressRow
                label={
                  p.phase === "hashing"
                    ? "Hashing"
                    : p.phase === "probing"
                      ? "Probing"
                      : p.phase === "copying"
                        ? "Copying"
                        : "Adding"
                }
                status="working"
                fraction={p.fraction}
              />
            </Card>
          ))}
        </div>
      )}
      {drawer && (
        <SourceDrawer
          model={drawer}
          speechReady={speechReady}
          onClose={() => selectSource(null)}
          onCopy={() => void copyHash()}
          onReveal={() => void reveal(drawer.row.path)}
          onTranscribe={() => void runTranscribe(drawer.row)}
          onAlignMic={() => void alignMic(drawer.row, "Choose the separate microphone recording")}
        />
      )}
    </PageShell>
  );
}

function TranscriptChip({ row, speechReady }: { row: VideoSourceRow; speechReady: boolean | null }) {
  if (row.has_transcript) return <Badge tone="ok">Transcribed</Badge>;
  if (speechReady === false) return <Badge tone="warn">Speech model unavailable</Badge>;
  return <Badge tone="neutral">Not transcribed</Badge>;
}

function SourceDrawer({
  model,
  speechReady,
  onClose,
  onCopy,
  onReveal,
  onTranscribe,
  onAlignMic,
}: {
  model: NonNullable<ReturnType<typeof useVideoSources>["drawer"]>;
  speechReady: boolean | null;
  onClose: () => void;
  onCopy: () => void;
  onReveal: () => void;
  onTranscribe: () => void;
  onAlignMic: () => void;
}) {
  const { row, probe, dimensions, fps, durationMs, hasAudio, playbackId, kind, canAlign, canTranscribe, excerpt, microphone, copied } = model;
  const meta: [string, string, boolean?][] = [
    ["Duration", timecode(durationMs)],
    [
      "Resolution",
      dimensions ? `${dimensions.width}×${dimensions.height}` : "—",
    ],
    ["Frame rate", fps ? `${fps.toFixed(fps % 1 ? 2 : 0)} fps` : "—"],
    ["Video codec", probe?.video_codec || "—"],
    ["Audio", probe?.audio_codec ? `${probe.audio_codec}${probe.audio_channels ? ` · ${probe.audio_channels}ch` : ""}` : hasAudio ? "yes" : "none"],
    ["Colour space", probe?.color_space ?? "—"],
    ["Size", row.size ? fileSize(row.size) : "—"],
  ];

  return (
    <Drawer
      title={row.name || baseName(row.path)}
      open
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onReveal}><Icon name="folder-open" />
            Reveal in Finder
          </Button>
          {!row.has_transcript ? (
            <Button
              variant="primary"
              disabled={!canTranscribe}
              title={speechReady === false ? "Speech model unavailable" : undefined}
              onClick={onTranscribe}
            ><Icon name="mic" />
              Transcribe
            </Button>
          ) : (
            <Button variant="secondary" disabled={!canTranscribe} onClick={onTranscribe}><Icon name="rotate-ccw" />
              Re-transcribe
            </Button>
          )}
          <Button
            variant="secondary"
            disabled={!canAlign}
            title={!row.exists ? "Relink this file before aligning a microphone" : undefined}
            onClick={onAlignMic}
          ><Icon name="music" />
            Align separate microphone
          </Button>
        </>
      }
    >
      {row.exists ? (
        <MediaPlayer
          key={playbackId ?? "pending"}
          src={row.path}
          playbackId={playbackId}
          kind={kind}
          title={row.name}
        />
      ) : (
        <p style={{ margin: 0, color: "var(--rk-warn)", fontSize: 13 }}>This file is missing — relink it to play.</p>
      )}
      <dl className="media-meta">
        {meta.map(([k, v]) => (
          <span key={k} style={{ display: "contents" }}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </span>
        ))}
        <dt>Hash</dt>
        <dd className="mono" title={row.id}>
          {row.id.slice(0, 12)}…{" "}
          <Button size="sm" variant="ghost" onClick={onCopy}><Icon name={copied ? "check" : "more-horizontal"} />
            {copied ? "Copied" : "Copy"}
          </Button>
        </dd>
        <dt>Path</dt>
        <dd title={row.path} style={{ color: "var(--rk-ink-2)" }}>
          {row.path}
        </dd>
      </dl>
      {microphone && <MicAlignStatus state={microphone} />}
      {row.has_transcript ? (
        <p className="media-excerpt">{excerpt ?? "Loading transcript…"}</p>
      ) : (
        <p className="media-excerpt">
          {speechReady === false
            ? "The speech model isn't installed, so this source can't be transcribed yet."
            : "No transcript yet — transcribe to enable word-level editing."}
        </p>
      )}
    </Drawer>
  );
}

/** Progress, failure, and the recorded offset for "Align separate microphone". */
function MicAlignStatus({ state }: { state: VideoMicState }) {
  if (state.status === "importing") {
    return <ProgressRow label="Adding microphone recording" status="working" fraction={undefined} />;
  }
  if (state.status === "aligning") {
    return <ProgressRow label="Aligning microphone to camera" status="working" fraction={undefined} />;
  }
  if (state.status === "failed") {
    return (
      <p role="alert" style={{ margin: 0, color: "var(--rk-bad)", fontSize: 13 }}>
        Microphone alignment failed — {state.message}
      </p>
    );
  }
  const { alignment, status } = state.result;
  const ms = alignment.offset_ms;
  const sign = ms < 0 ? "−" : "+";
  return (
    <p role="status" style={{ margin: 0, fontSize: 13, color: "var(--rk-ink-2)" }}>
      Microphone offset {sign}
      {(Math.abs(ms) / 1000).toFixed(3)} s (mic time minus camera time) · confidence{" "}
      {alignment.confidence.toFixed(2)}
      {status === "already_attached" ? " · mapping already recorded" : ""}
    </p>
  );
}
