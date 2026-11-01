import { Badge, Button, Card, EmptyState, PageShell } from "@rightkit/react-shell/react";
import "./Media.css";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { pickFile, pickMedia } from "../lib/dialog";
import { engineCall, onEngineRaw, errorMessage, onEngineEvent, revealPath } from "../lib/engine";
import { MediaPlayer } from "@rightkit/media-ui";
import { mediaKind, useMediaPlaybackId } from "../lib/mediaAdapters";
import { type MicAlignResult, micAlign } from "../lib/mic";
import { TICKS_PER_SECOND, baseName, projectShow, type ProjectShow } from "../lib/project";
import {
  type SourceProbe,
  modelsStatus,
  settingsGet,
  settingsSet,
  sourceList,
  sourceProbe,
  sourceRelink,
  sourceVerify,
  thumbnails,
  transcribe,
  transcriptShow,
} from "../lib/media";
import { assetUrl } from "../lib/asset";
import { Drawer, Icon, ProgressRow, Thumbnail, fileSize, timecode } from "../ui";

type Row = {
  id: string;
  name: string;
  mode?: string;
  path: string;
  size?: number;
  exists: boolean;
  has_transcript: boolean;
};

type VerifyStatus = "ok" | "missing" | "changed";
type Intent = "talking-head" | "mixed" | "visual";
type Busy = "importing" | "hashing" | "transcribing";

type SourceVerify = {
  ok: boolean;
  checks: { source_id: string; status: string; found_at?: string | null }[];
};

const INTENT_LABEL: Record<Intent, string> = {
  "talking-head": "Talking head",
  mixed: "Mixed",
  visual: "Visual",
};
const INTENT_ORDER: Intent[] = ["talking-head", "mixed", "visual"];

/** Audio recorders' files (and camera containers carrying audio) for a separate mic. */
const MIC_EXTENSIONS = ["wav", "mp3", "m4a", "flac", "aac", "ogg", "mp4", "mov"];

/** Per-camera state of the "Align separate microphone" action. */
type MicState =
  | { status: "importing" }
  | { status: "aligning" }
  | { status: "done"; result: MicAlignResult }
  | { status: "failed"; message: string };

/** §3.2 Media: source cards, drag-drop import, per-card pipeline state, detail drawer. */
export function SourcesView({
  dir,
  onChanged,
  onNavigate,
}: {
  dir: string;
  onChanged: () => Promise<void>;
  onNavigate?: (to: "recipe" | "finished") => void;
}) {
  const [rows, setRows] = useState<Row[]>([]);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [probes, setProbes] = useState<Record<string, SourceProbe>>({});
  const [checks, setChecks] = useState<Record<string, VerifyStatus>>({});
  const [verifiedCount, setVerifiedCount] = useState<number | null>(null);
  const [transcribing, setTranscribing] = useState<Set<string>>(new Set());
  // Live transcription stage per source ("Queued for the host app: dictation in progress", …).
  const [txStatus, setTxStatus] = useState<Record<string, { message: string; fraction?: number }>>({});
  useEffect(() => {
    let off: (() => void) | undefined;
    let dead = false;
    void onEngineEvent((frame) => {
      const ev = frame.event as { event?: string; source?: string; message?: string; fraction?: number };
      if (!frame.tag?.startsWith("ui-tx-") || ev.event !== "transcribe.progress" || !ev.source) return;
      const source = ev.source;
      setTxStatus((m) => ({ ...m, [source]: { message: ev.message ?? "Transcribing", fraction: ev.fraction || undefined } }));
    }).then((fn) => {
      if (dead) fn();
      else off = fn;
    });
    return () => {
      dead = true;
      off?.();
    };
  }, []);
  const [importing, setImporting] = useState(0);
  /** Files whose source.add is in flight; late progress events for others are dropped. */
  const adding = useRef(new Set<string>());
  // §3.2: per-file source.add progress (hashing/copying/probing fractions).
  const [addProgress, setAddProgress] = useState<
    Record<string, { phase: string; fraction?: number }>
  >({});
  const [speechReady, setSpeechReady] = useState<boolean | null>(null);
  const [intents, setIntents] = useState<Record<string, Intent>>({});
  const [error, setError] = useState<string | null>(null);
  const [micState, setMicState] = useState<Record<string, MicState>>({});
  const [dropActive, setDropActive] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [thumbSrc, setThumbSrc] = useState<Record<string, string>>({});
  const thumbsRequested = useRef<Set<string>>(new Set());

  const infoOf = useCallback(
    (id: string) => show?.timeline.sources.find((s) => s.id === id),
    [show],
  );

  const intentOf = useCallback(
    (r: Row): Intent => {
      const o = intents[r.id];
      if (o) return o;
      const hasAudio = infoOf(r.id)?.has_audio ?? false;
      if (!hasAudio) return "visual";
      return r.has_transcript ? "talking-head" : "mixed";
    },
    [intents, infoOf],
  );

  const refresh = useCallback(async () => {
    const [list, proj, settings, models] = await Promise.all([
      sourceList(dir),
      projectShow(dir).catch(() => null),
      settingsGet(dir).catch(() => null),
      modelsStatus().catch(() => null),
    ]);
    setRows(list.sources as Row[]);
    setShow(proj);
    setSpeechReady(models ? (models.speech.ready ?? null) : null);
    const saved = settings?.project?.intents;
    if (saved && typeof saved === "object") setIntents(saved as Record<string, Intent>);
    else setIntents({});
  }, [dir]);

  useEffect(() => {
    refresh().catch((e) => setError(errorMessage(e)));
  }, [refresh]);

  // Integrity check on open: per-card hashing state until its check lands.
  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    void onEngineRaw((payload) => {
      const ev = payload?.event as unknown as { event?: string; completed?: number };
      if (ev?.event === "source.verify" && typeof ev.completed === "number") {
        setVerifiedCount(ev.completed);
      }
    }).then((f) => {
      if (cancelled) f();
      else off = f;
    });
    sourceVerify(dir, `media-verify-${dir}`)
      .then((r: SourceVerify) => {
        if (cancelled) return;
        const map: Record<string, VerifyStatus> = {};
        for (const c of r.checks ?? []) {
          map[c.source_id] =
            c.status === "ok"
              ? "ok"
              : c.status === "missing" || c.status === "unreadable"
                ? "missing"
                : "changed"; // mismatch | moved
        }
        setChecks(map);
        setVerifiedCount(null);
      })
      .catch(() => setVerifiedCount(null));
    return () => {
      cancelled = true;
      off?.();
    };
  }, [dir]);

  // Transcription pipeline state (own calls and recipe-run stages alike).
  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    void onEngineRaw((payload) => {
      const ev = (
        payload as unknown as {
          event?: { event?: string; source?: string; file?: string; phase?: string; fraction?: number };
        }
      ).event;
      if (ev?.event === "source.add.progress" && ev.file) {
        const { file, phase = "working", fraction } = ev;
        // Events can trail the source.add response; only files still in flight count.
        if (!adding.current.has(file)) return;
        setAddProgress((m) => ({ ...m, [file]: { phase, fraction } }));
        return;
      }
      const sid = ev?.source;
      if (!sid) return;
      if (ev?.event === "transcribe.started") {
        setTranscribing((s) => new Set(s).add(sid));
      } else if (ev?.event === "transcribe.done") {
        setTranscribing((s) => {
          const n = new Set(s);
          n.delete(sid);
          return n;
        });
        void refresh();
      }
    }).then((f) => {
      if (cancelled) f();
      else off = f;
    });
    return () => {
      cancelled = true;
      off?.();
    };
  }, [refresh]);

  // Real thumbnails + probe metadata per card (cached engine-side).
  useEffect(() => {
    for (const r of rows) {
      if (!r.exists || thumbsRequested.current.has(r.id)) continue;
      thumbsRequested.current.add(r.id);
      void thumbnails(dir, { source: r.id }, { width: 480 })
        .then((t) => {
          const frame = t.frames[0];
          if (frame) setThumbSrc((m) => ({ ...m, [r.id]: assetUrl(frame.path) }));
        })
        .catch(() => undefined);
      void sourceProbe(dir, r.id)
        .then((p) => setProbes((m) => ({ ...m, [r.id]: p })))
        .catch(() => undefined);
    }
  }, [rows, dir]);

  const busyOf = useCallback(
    (r: Row, index: number): Busy | null => {
      if (transcribing.has(r.id)) return "transcribing";
      if (verifiedCount !== null && index >= verifiedCount && r.exists) return "hashing";
      return null;
    },
    [transcribing, verifiedCount],
  );

  const importFiles = useCallback(
    async (files: string[]) => {
      if (files.length === 0) return;
      setImporting((n) => n + 1);
      setError(null);
      try {
        for (const file of files) {
          adding.current.add(file);
          setAddProgress((m) => ({ ...m, [file]: { phase: "hashing", fraction: undefined } }));
          try {
            await engineCall("source.add", { dir, file });
          } finally {
            adding.current.delete(file);
            setAddProgress((m) => {
              const n = { ...m };
              delete n[file];
              return n;
            });
          }
        }
        await refresh();
        await onChanged();
      } catch (e) {
        setError(errorMessage(e));
      } finally {
        setImporting((n) => n - 1);
      }
    },
    [dir, refresh, onChanged],
  );

  const importMedia = useCallback(async () => {
    await importFiles(await pickMedia());
  }, [importFiles]);

  // Drag-and-drop import anywhere on the screen.
  useEffect(() => {
    let depth = 0;
    const exts = /\.(mp4|mov|mkv|webm|m4v|wav|mp3|m4a|flac|aac|ogg)$/i;
    const onDrag = (e: DragEvent) => {
      if (!e.dataTransfer?.types.includes("Files")) return;
      e.preventDefault();
      if (e.type === "dragenter") depth += 1;
      if (e.type === "dragleave") depth -= 1;
      if (e.type === "drop") depth = 0;
      setDropActive(depth > 0);
    };
    const onDrop = (e: DragEvent) => {
      e.preventDefault();
      const files = [...(e.dataTransfer?.files ?? [])]
        .map((f) => (f as File & { path?: string }).path ?? "")
        .filter((p) => exts.test(p));
      if (files.length) void importFiles(files);
    };
    window.addEventListener("dragenter", onDrag);
    window.addEventListener("dragleave", onDrag);
    window.addEventListener("dragover", onDrag);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragenter", onDrag);
      window.removeEventListener("dragleave", onDrag);
      window.removeEventListener("dragover", onDrag);
      window.removeEventListener("drop", onDrop);
    };
  }, [importFiles]);

  const relink = useCallback(
    async (r: Row) => {
      const picked = await pickMedia();
      const path = picked[0];
      if (!path) return;
      try {
        await sourceRelink(dir, r.id, path);
        thumbsRequested.current.delete(r.id);
        await refresh();
        await onChanged();
      } catch (e) {
        setError(errorMessage(e));
      }
    },
    [dir, refresh, onChanged],
  );

  const cycleIntent = useCallback(
    (r: Row) => {
      const next =
        INTENT_ORDER[(INTENT_ORDER.indexOf(intentOf(r)) + 1) % INTENT_ORDER.length];
      const updated = { ...intents, [r.id]: next };
      setIntents(updated);
      settingsSet({ intents: updated }, dir).catch((e) => setError(errorMessage(e)));
    },
    [dir, intents, intentOf],
  );

  /** Picks a separate mic recording, registers it with source.add, then aligns it to `r`. */
  const alignMic = useCallback(
    async (r: Row) => {
      const file = await pickFile("Choose the separate microphone recording", MIC_EXTENSIONS);
      if (!file) return;
      const setFor = (state: MicState) => setMicState((m) => ({ ...m, [r.id]: state }));
      setFor({ status: "importing" });
      try {
        const added = await engineCall<{ source: { id: string } }>("source.add", { dir, file });
        await refresh();
        setFor({ status: "aligning" });
        const result = await micAlign(dir, r.id, added.source.id, {}, `ui-mic-${r.id.slice(0, 8)}`);
        setFor({ status: "done", result });
        await onChanged();
      } catch (e) {
        setFor({ status: "failed", message: errorMessage(e) });
      }
    },
    [dir, refresh, onChanged],
  );

  const runTranscribe = useCallback(
    async (r: Row) => {
      setError(null);
      setTranscribing((s) => new Set(s).add(r.id));
      try {
        const job = `ui-tx-${r.id.slice(0, 8)}`;
        await transcribe(dir, r.id, job, true, job);
        await refresh();
      } catch (e) {
        setError(errorMessage(e));
      } finally {
        setTranscribing((s) => {
          const n = new Set(s);
          n.delete(r.id);
          return n;
        });
        setTxStatus(({ [r.id]: _done, ...rest }) => rest);
      }
    },
    [dir, refresh],
  );

  const totalMs = useMemo(
    () =>
      rows.reduce((ms, r) => ms + ((infoOf(r.id)?.duration ?? 0) / TICKS_PER_SECOND) * 1000, 0),
    [rows, infoOf],
  );
  const readyCount = rows.filter((r) => r.exists && checks[r.id] !== "missing" && checks[r.id] !== "changed").length;
  const openRow = rows.find((r) => r.id === open) ?? null;

  return (
    <PageShell title="Media" actions={
        <>
          <Button variant="secondary" onClick={() => void importMedia()}><Icon name="upload" />
            Import…
          </Button>
          <Button
            variant="primary"
            disabled={readyCount === 0}
            title={readyCount === 0 ? "Import at least one source first" : undefined}
            onClick={() => onNavigate?.("recipe")}
          ><Icon name="wand-sparkles" />
            Make finished edit
          </Button>
        </>
      } maxWidth={0} subtitle={rows.length ? `${rows.length} clips · ${timecode(totalMs)} total` : undefined}>
      {dropActive && (
        <div className="media-drop" aria-hidden>
          <span>Drop to add</span>
        </div>
      )}
      {error && (
        <p role="alert" style={{ margin: 0, color: "var(--rk-bad)", fontSize: 13 }}>
          {error}
        </p>
      )}
      {importing > 0 && <ProgressRow label="Importing" status="working" fraction={undefined} />}
      {rows.length === 0 && importing === 0 ? (
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
          {rows.map((r, i) => {
            const busy = busyOf(r, i);
            const check = checks[r.id];
            const missing = check === "missing" || !r.exists;
            const changed = check === "changed";
            const info = infoOf(r.id);
            const probe = probes[r.id];
            const fps = probe?.fps ? Math.round(probe.fps.num / probe.fps.den) : null;
            const dims = probe
              ? `${probe.width}×${probe.height}`
              : info?.video
                ? `${info.video.width}×${info.video.height}`
                : null;
            const durMs = probe?.duration_ms ?? ((info?.duration ?? 0) / TICKS_PER_SECOND) * 1000;
            const intent = intentOf(r);
            return (
              <Card key={r.id} onClick={() => setOpen(r.id)} label={r.name || baseName(r.path)} className="media-card">
                <Thumbnail
                  src={missing || changed ? undefined : thumbSrc[r.id]}
                  badge={durMs ? timecode(durMs) : undefined}
                  video={!missing && !changed && r.exists ? assetUrl(r.path) : undefined}
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
                  {[dims, fps ? `${fps} fps` : null, info?.has_audio === false ? "No audio" : "Audio"]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
                {busy ? (
                  <ProgressRow
                    label={
                      busy === "hashing"
                        ? "Hashing"
                        : busy === "transcribing"
                          ? (txStatus[r.id]?.message ?? "Transcribing")
                          : "Probing"
                    }
                    status="working"
                    fraction={busy === "transcribing" ? txStatus[r.id]?.fraction : undefined}
                  />
                ) : (
                  // Chips act on their own; they must not also open the card's drawer.
                  <span
                    className="media-card__chips"
                    onClick={(e) => e.stopPropagation()}
                    onKeyDown={(e) => e.stopPropagation()}
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
      {openRow != null && (
        <SourceDrawer
          dir={dir}
          row={openRow}
          probe={probes[openRow.id]}
          info={infoOf(openRow.id)}
          speechReady={speechReady}
          onClose={() => setOpen(null)}
          onTranscribe={() => void runTranscribe(openRow)}
          mic={micState[openRow.id]}
          onAlignMic={() => void alignMic(openRow)}
        />
      )}
    </PageShell>
  );
}

function TranscriptChip({ row, speechReady }: { row: Row; speechReady: boolean | null }) {
  if (row.has_transcript) return <Badge tone="ok">Transcribed</Badge>;
  if (speechReady === false) return <Badge tone="warn">Speech model unavailable</Badge>;
  return <Badge tone="neutral">Not transcribed</Badge>;
}

function SourceDrawer({
  dir,
  row,
  probe,
  info,
  speechReady,
  onClose,
  onTranscribe,
  mic,
  onAlignMic,
}: {
  dir: string;
  row: Row;
  probe?: SourceProbe;
  info?: { duration: number; video: { width: number; height: number } | null; has_audio: boolean };
  speechReady: boolean | null;
  onClose: () => void;
  onTranscribe: () => void;
  mic?: MicState;
  onAlignMic: () => void;
}) {
  const micBusy = mic?.status === "importing" || mic?.status === "aligning";
  const playbackId = useMediaPlaybackId(row.exists ? row.path : null);
  const [excerpt, setExcerpt] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const durMs = probe?.duration_ms ?? ((info?.duration ?? 0) / TICKS_PER_SECOND) * 1000;
  const fps = probe?.fps ? probe.fps.num / probe.fps.den : null;

  useEffect(() => {
    if (!row.has_transcript) return;
    transcriptShow(dir, row.id)
      .then((t) =>
        setExcerpt(
          t.transcript.words
            .slice(0, 60)
            .map((w) => w.text)
            .join(" "),
        ),
      )
      .catch(() => setExcerpt(null));
  }, [dir, row]);

  const copyHash = () => {
    void navigator.clipboard?.writeText(row.id).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };

  const meta: [string, string, boolean?][] = [
    ["Duration", timecode(durMs)],
    [
      "Resolution",
      probe ? `${probe.width}×${probe.height}` : info?.video ? `${info.video.width}×${info.video.height}` : "—",
    ],
    ["Frame rate", fps ? `${fps.toFixed(fps % 1 ? 2 : 0)} fps` : "—"],
    ["Video codec", probe?.video_codec || "—"],
    ["Audio", probe?.audio_codec ? `${probe.audio_codec}${probe.audio_channels ? ` · ${probe.audio_channels}ch` : ""}` : info?.has_audio ? "yes" : "none"],
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
          <Button variant="ghost" onClick={() => void revealPath(row.path)}><Icon name="folder-open" />
            Reveal in Finder
          </Button>
          {!row.has_transcript ? (
            <Button
              variant="primary"
              disabled={speechReady === false}
              title={speechReady === false ? "Speech model unavailable" : undefined}
              onClick={onTranscribe}
            ><Icon name="mic" />
              Transcribe
            </Button>
          ) : (
            <Button variant="secondary" disabled={speechReady === false} onClick={onTranscribe}><Icon name="rotate-ccw" />
              Re-transcribe
            </Button>
          )}
          <Button
            variant="secondary"
            disabled={micBusy || !row.exists}
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
          kind={mediaKind(row.path)}
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
          <Button size="sm" variant="ghost" onClick={copyHash}><Icon name={copied ? "check" : "more-horizontal"} />
            {copied ? "Copied" : "Copy"}
          </Button>
        </dd>
        <dt>Path</dt>
        <dd title={row.path} style={{ color: "var(--rk-ink-2)" }}>
          {row.path}
        </dd>
      </dl>
      {mic && <MicAlignStatus state={mic} />}
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
function MicAlignStatus({ state }: { state: MicState }) {
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
