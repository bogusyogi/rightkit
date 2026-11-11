import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import {
  assetUrl,
  errorMessage,
  fetchJobStatus,
  isHeldJob,
  toError,
  type GenerateRequest,
  type JobStatus,
  type MediaKind
} from '../lib/api.js';
import { mediaGenClient } from '../lib/client.js';
import { jobIsActive, useApp, type SessionJob } from '../lib/app-state.js';
import { JobOutputs, statusText } from '../components/JobCard.js';
import { heldActionLabel, heldActions, heldConsequence, heldExplanation, heldLabel, isRecoveredCode, type HeldAction } from '../lib/held-job.js';
import { useConfirm } from '../lib/confirm.js';
import { ErrorNote } from '../components/ErrorNote.js';
import { IconAvatar, IconImage, IconMic, IconMotion, IconSpeaker, IconVideo } from '../components/icons.js';
import { Card, Badge, Button, EmptyState, Notice, SegmentedControl } from '@rightkit/react-shell/react';
import { relativeTime, usd } from '../components/ui.js';

const KIND_ICON: Record<MediaKind, ReactNode> = {
  image: <IconImage />,
  video: <IconVideo />,
  voice: <IconMic />,
  avatar: <IconAvatar />,
  music: <IconSpeaker />,
  sfx: <IconMotion />,
  upscale: <IconImage />,
  lipsync: <IconAvatar />
};

const KINDS: readonly MediaKind[] = ['image', 'video', 'voice', 'avatar', 'music', 'sfx', 'upscale', 'lipsync'];
/** Kinds whose finished asset is a still image (thumbnail-able). */
const IMAGE_KINDS = new Set<string>(['image', 'upscale']);

type Filter = 'all' | 'active' | 'unknown' | 'done' | 'failed' | 'cancelled';
const FILTERS: Array<{ id: Filter; label: string }> = [
  { id: 'all', label: 'All' },
  { id: 'active', label: 'Active' },
  { id: 'unknown', label: 'Outcome unknown' },
  { id: 'done', label: 'Done' },
  { id: 'failed', label: 'Failed' },
  { id: 'cancelled', label: 'Cancelled' }
];

/** A non-terminal job with no observed progress for this long is stale: its
 * runner stopped (crash, closed window) and nothing is driving it. */
const STALE_MS = (() => {
  if (!import.meta.env?.DEV) return 90_000;
  try {
    const v = Number(new URLSearchParams(window.location.search).get('qaStaleMs'));
    return Number.isFinite(v) && v > 0 ? v : 90_000;
  } catch {
    return 90_000;
  }
})();

function bucket(job: SessionJob): Filter {
  if (jobIsActive(job)) return 'active';
  if (isHeldJob(job.status)) return 'unknown';
  if (job.status === 'succeeded') return 'done';
  if (job.status === 'cancelled') return 'cancelled';
  return 'failed';
}

/** Engine `job.run` resumes a non-terminal job: the calling process becomes
 * its runner, picking up at the stored provider job id or resubmitting.
 * Kept here so this view has no dependency beyond the command surface. */
async function runEngineJob(jobId: string): Promise<JobStatus> {
  const input = { job_id: jobId };
  try {
    return await mediaGenClient().request<JobStatus>('command', { name: 'job.run', input });
  } catch (error) {
    throw toError(error);
  }
}

/** The stored engine request, for resubmitting a finished job as a new one. */
function storedRequest(status: JobStatus): GenerateRequest | null {
  const raw = status as JobStatus & { request?: Partial<GenerateRequest> };
  const r = raw.request;
  if (!r || !r.provider || !r.model || typeof r.prompt !== 'string') return null;
  const kind = r.kind && KINDS.includes(r.kind as MediaKind) ? (r.kind as MediaKind) : undefined;
  if (!kind) return null;
  return { kind, provider: r.provider, model: r.model, prompt: r.prompt, params: r.params };
}

export function Jobs() {
  const app = useApp();
  const confirm = useConfirm();
  const [refreshing, setRefreshing] = useState(false);
  const [filter, setFilter] = useState<Filter>('all');
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState<Record<string, 'resume' | 'retry' | 'cancel' | 'resolve'>>({});
  const [actionError, setActionError] = useState<Record<string, string>>({});
  const [now, setNow] = useState(() => Date.now());
  const seen = useRef(new Map<string, { fp: string; at: number }>());

  const jobs = app.jobs;
  const active = jobs.filter(jobIsActive).length;

  // Liveness fingerprint: a job is alive while its visible state keeps changing.
  useEffect(() => {
    const t = Date.now();
    for (const j of jobs) {
      const fp = `${j.status}|${j.stage ?? ''}|${j.stageMessage ?? ''}|${j.progress ?? ''}|${j.assets.length}`;
      const prev = seen.current.get(j.key);
      if (!prev || prev.fp !== fp) seen.current.set(j.key, { fp, at: t });
    }
    for (const key of [...seen.current.keys()]) {
      if (!jobs.some((j) => j.key === key)) seen.current.delete(key);
    }
  }, [jobs]);

  // Tick so staleness updates while anything is active.
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active]);

  const staleKeys = useMemo(() => {
    const set = new Set<string>();
    for (const j of jobs) {
      if (j.source !== 'cloud' || !jobIsActive(j)) continue;
      const at = seen.current.get(j.key)?.at ?? now;
      if (now - at >= STALE_MS) set.add(j.key);
    }
    return set;
  }, [jobs, now]);

  const filtered = useMemo(
    () => jobs.filter((j) => filter === 'all' || bucket(j) === filter),
    [jobs, filter]
  );

  const setJobError = (key: string, msg: string | null) =>
    setActionError((m) => {
      const next = { ...m };
      if (msg) next[key] = msg;
      else delete next[key];
      return next;
    });

  const markBusy = (key: string, what: 'resume' | 'retry' | 'cancel' | 'resolve' | null) =>
    setBusy((m) => {
      const next = { ...m };
      if (what) next[key] = what;
      else delete next[key];
      return next;
    });

  const resume = async (job: SessionJob) => {
    markBusy(job.key, 'resume');
    setJobError(job.key, null);
    try {
      await runEngineJob(job.jobId);
      seen.current.set(job.key, { fp: '', at: Date.now() });
      await app.refreshHistory();
    } catch (e) {
      setJobError(job.key, errorMessage(e));
    } finally {
      markBusy(job.key, null);
    }
  };

  const resumeAll = async () => {
    for (const j of jobs) {
      if (staleKeys.has(j.key)) void resume(j);
    }
  };

  const retry = async (job: SessionJob) => {
    markBusy(job.key, 'retry');
    setJobError(job.key, null);
    try {
      const status = await fetchJobStatus(job.jobId);
      const request = storedRequest(status);
      if (!request) throw new Error('The stored request for this job is not readable; regenerate it from the Create view.');
      await app.submitCloud({ ...request, request_id: crypto.randomUUID() }, job.label, job.aspect);
    } catch (e) {
      setJobError(job.key, errorMessage(e));
    } finally {
      markBusy(job.key, null);
    }
  };

  const resolveHeld = async (job: SessionJob, action: HeldAction) => {
    if (!(await confirm({ title: heldActionLabel(action), paragraphs: [heldConsequence(action)], confirmLabel: heldActionLabel(action) }))) return;
    markBusy(job.key, 'resolve');
    setJobError(job.key, null);
    try {
      await app.resolveHeld(job, action);
    } catch (e) {
      setJobError(job.key, errorMessage(e));
    } finally {
      markBusy(job.key, null);
    }
  };

  const cancel = async (job: SessionJob) => {
    markBusy(job.key, 'cancel');
    setJobError(job.key, null);
    try {
      await app.cancel(job);
    } catch (e) {
      setJobError(job.key, errorMessage(e));
    } finally {
      markBusy(job.key, null);
    }
  };

  const counts = useMemo(() => {
    const c: Record<Filter, number> = { all: jobs.length, active: 0, unknown: 0, done: 0, failed: 0, cancelled: 0 };
    for (const j of jobs) c[bucket(j)] += 1;
    return c;
  }, [jobs]);

  return (
    <div className="content view" style={{ gridTemplateColumns: '1fr' }}>
      <section className="work">
        <div className="img-head">
          <div><div className="eyebrow">Library</div><h1>Jobs</h1></div>
          <Button
            type="button"
            variant="secondary"
            disabled={refreshing}
            onClick={() => {
              setRefreshing(true);
              void app.refreshHistory().finally(() => setRefreshing(false));
            }}
          >{refreshing ? 'Refreshing…' : 'Refresh history'}</Button>
        </div>
        <p className="lede" style={{ marginBottom: 14 }}>
          {active ? `${active} running now. ` : ''}Generation history from this session and the engine. Finished results can be acknowledged by other tools and cleaned up later.
        </p>

        <div className="toolbar" style={{ marginBottom: 12 }}>
          <SegmentedControl
            label="Filter jobs"
            value={filter}
            onChange={setFilter}
            options={FILTERS.map((f) => ({ value: f.id, label: counts[f.id] ? `${f.label} ${counts[f.id]}` : f.label }))}
          />
        </div>

        {staleKeys.size ? (
          <Notice
            tone="warn"
            action={<Button type="button" variant="secondary" size="sm" onClick={() => void resumeAll()}>Resume all</Button>}
          >
            {staleKeys.size} job{staleKeys.size === 1 ? '' : 's'} stopped mid-run — no progress for over {Math.round(STALE_MS / 1000)}s. Resuming makes this app the runner again.
          </Notice>
        ) : null}

        {jobs.length === 0 ? (
          <EmptyState title="No jobs yet">Generate an image, clip, or voice line and it will be listed here.</EmptyState>
        ) : filtered.length === 0 ? (
          <EmptyState title="Nothing in this state">Switch the filter to see the rest of the queue.</EmptyState>
        ) : (
          <Card flush className="row-list">
            {filtered.map((j) => {
              const first = j.assets[0];
              const isActive = jobIsActive(j);
              const stale = staleKeys.has(j.key);
              const b = busy[j.key];
              const held = isHeldJob(j.status);
              const tone = j.status === 'succeeded' ? 'ok' : held || stale ? 'warn' : isActive ? 'accent' : j.status === 'cancelled' ? 'neutral' : 'bad';
              const expanded = open === j.key;
              return (
                <div className={`jitem${expanded ? ' open' : ''}`} key={j.key}>
                  <div className="jrow">
                    <div className="th">{first && IMAGE_KINDS.has(first.media || j.kind) ? <img src={assetUrl(first.path)} alt="" /> : KIND_ICON[j.kind]}</div>
                    <div>
                      <b>{j.prompt || j.label}</b>
                      <small>
                        {j.label} {'·'} {j.provider || 'local'} {'·'} {relativeTime(j.createdAt)}
                        {j.source === 'cloud' && j.estUsd ? ` · ${isActive ? 'est. ' : ''}${usd(j.estUsd)}` : ' · local'}
                      </small>
                    </div>
                    <span title={j.error ?? undefined}>
                      <Badge tone={tone}>
                        {held ? heldLabel(isRecoveredCode(j.errorCode)) : stale ? 'Stopped' : j.status === 'succeeded' ? 'Done' : j.status === 'failed' && j.errorCode ? j.errorCode : j.status === 'cancelled' ? 'Cancelled' : statusText(j).slice(0, 40)}
                      </Badge>
                    </span>
                    <Button
                      type="button"
                      variant="secondary"
                      size="sm"
                      aria-expanded={expanded}
                      aria-label={`${expanded ? 'Hide' : 'Show'} details for ${j.label}`}
                      onClick={() => setOpen(expanded ? null : j.key)}
                    >{expanded ? 'Hide' : 'Details'}</Button>
                  </div>
                  {expanded ? (
                    <div className="jdetail">
                      <div className="jmeta">
                        <span>Job <code className="selectable">{j.jobId}</code></span>
                        <span>{j.kind} · {j.provider || 'local'}{j.model && j.model !== j.provider ? ` · ${j.model}` : ''}</span>
                        <span>{relativeTime(j.createdAt)}</span>
                        {j.stage && isActive ? <span>{statusText(j)}</span> : null}
                      </div>
                      {stale ? (
                        <Notice tone="warn">
                          No progress observed for over {Math.round(STALE_MS / 1000)}s — its runner likely stopped. Resume drives it from this app; Cancel ends it.
                        </Notice>
                      ) : null}
                      {held ? (
                        <Notice tone="warn">{heldExplanation(j.provider, isRecoveredCode(j.errorCode))}</Notice>
                      ) : null}
                      {j.error || actionError[j.key] ? (
                        <ErrorNote code={j.errorCode ?? undefined} message={actionError[j.key] ?? j.errorMessage ?? j.error ?? 'Failed'} />
                      ) : null}
                      <JobOutputs job={j} onRetry={j.source === 'cloud' && !isActive ? () => void retry(j) : undefined} />
                      {j.status === 'succeeded' && j.assets.length ? (
                        <div className="jhashes">
                          {j.assets.map((a) => (
                            <div className="jhash" key={a.asset_id}>
                              <span className="mono selectable" title={a.asset_id}>{a.asset_id}</span>
                              <code className="selectable" title={a.sha256 ?? 'hash pending'}>{a.sha256 ? `sha256 ${a.sha256.slice(0, 16)}…` : 'sha256 pending'}</code>
                            </div>
                          ))}
                        </div>
                      ) : null}
                      <div className="jacts">
                        {isActive ? (
                          <>
                            {stale && j.source === 'cloud' ? (
                              <Button type="button" variant="secondary" size="sm" disabled={Boolean(b)} onClick={() => void resume(j)}>{b === 'resume' ? 'Resuming…' : 'Resume'}</Button>
                            ) : null}
                            <Button type="button" variant="danger" size="sm" disabled={Boolean(b)} onClick={() => void cancel(j)}>{b === 'cancel' ? 'Cancelling…' : 'Cancel'}</Button>
                          </>
                        ) : held ? (
                          <>
                            {heldActions(isRecoveredCode(j.errorCode)).map((a) => (
                              <Button key={a} type="button" variant={a === 'abandon' ? 'danger' : 'secondary'} size="sm" disabled={Boolean(b)} onClick={() => void resolveHeld(j, a)}>{b === 'resolve' && a !== 'abandon' ? 'Working…' : heldActionLabel(a)}</Button>
                            ))}
                          </>
                        ) : j.source === 'cloud' && j.status !== 'succeeded' ? (
                          <Button type="button" variant="secondary" size="sm" disabled={Boolean(b)} onClick={() => void retry(j)}>{b === 'retry' ? 'Submitting…' : 'Retry as new job'}</Button>
                        ) : null}
                      </div>
                    </div>
                  ) : null}
                </div>
              );
            })}
          </Card>
        )}
      </section>
    </div>
  );
}
