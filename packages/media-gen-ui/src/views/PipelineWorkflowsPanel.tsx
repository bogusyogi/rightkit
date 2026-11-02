import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  assetUrl, cancelJob, errorMessage, fetchJobResult, fetchJobStatus, isTerminalJob, isSuccessJob, submitGeneration
} from '../lib/api.js';
import {
  pipelinesApi, blockedReason, dubMaxTempoOf, dubReviewOf,
  type DubReview, type DubReviewChoice, type PipelineRun, type PipelineTemplate, type PipelineWorkflow, type RunAsset, type RunStep, type SegmentRequest
} from '../lib/pipeline-workflows.js';

import { Badge, Button, Card, Checkbox, EmptyState, Input, Notice, SegmentedControl, Table, TableBody, TableCell, TableHeaderCell, TableHead, TableRow } from '@rightkit/react-shell/react';
import { usd, relativeTime, baseName } from '../components/ui.js';
import { MediaPlayer } from '@rightkit/media-ui';
import { hostAppName } from "../appName.js";

type Mode = 'templates' | 'workflows' | 'runs';
const MODES: Array<{ id: Mode; label: string }> = [
  { id: 'templates', label: 'Templates' },
  { id: 'workflows', label: 'Workflows' },
  { id: 'runs', label: 'Runs' }
];

const STEP_TONE: Record<string, 'ok' | 'bad' | 'warn' | 'accent' | undefined> = {
  succeeded: 'ok',
  failed: 'bad',
  cancelled: 'bad',
  running: 'accent',
  awaiting_submit: 'accent',
  unsupported: 'warn',
  route_unready: 'warn',
  skipped: 'warn'
};

const SUPPORT_TONE: Record<string, 'ok' | 'bad' | 'warn' | 'accent' | undefined> = {
  ready: 'ok',
  native: 'ok',
  handoff: undefined,
  route_unready: 'warn',
  unsupported: 'bad'
};

function assetKindToPlayer(kind?: string, mime?: string): 'video' | 'audio' | null {
  if (kind === 'video') return 'video';
  if (kind === 'audio' || (mime ?? '').startsWith('audio/')) return 'audio';
  return null;
}

export function PipelineWorkflowsPanel() {
  const [mode, setMode] = useState<Mode>('templates');
  const [templates, setTemplates] = useState<PipelineTemplate[]>([]);
  const [workflows, setWorkflows] = useState<Array<{ id: string; name: string; template_id: string; steps: number; updated_at?: string }>>([]);
  const [runs, setRuns] = useState<Array<{ run_id: string; workflow_name: string; status: string; spent_usd: number; budget_usd: number | null; updated_at: string }>>([]);
  const [error, setError] = useState<string | null>(null);

  const refreshLists = useCallback(async () => {
    try {
      const [t, w, r] = await Promise.all([pipelinesApi.templates(), pipelinesApi.list(), pipelinesApi.runList()]);
      setTemplates(t.templates);
      setWorkflows(w.workflows);
      setRuns(r.runs);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);

  useEffect(() => {
    void refreshLists();
  }, [refreshLists]);

  return (
    <div className="pw">
      <div className="toolbar">
        <SegmentedControl label="Pipelines section" value={mode} onChange={setMode} options={MODES.map((m) => ({ value: m.id, label: m.label }))} />
        <Button type="button" variant="ghost" style={{ marginLeft: 'auto' }} onClick={() => void refreshLists()}>Refresh</Button>
      </div>
      {error ? <Notice tone="bad">{error}</Notice> : null}
      {mode === 'templates' ? (
        <TemplatesView templates={templates} onSaved={() => { void refreshLists(); setMode('workflows'); }} onRun={() => { void refreshLists(); setMode('runs'); }} onError={setError} />
      ) : null}
      {mode === 'workflows' ? <WorkflowsView workflows={workflows} onChanged={refreshLists} onRun={() => { void refreshLists(); setMode('runs'); }} onError={setError} /> : null}
      {mode === 'runs' ? <RunsView runs={runs} onChanged={refreshLists} onError={setError} /> : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// templates: pick a donor pipeline, fill inputs, plan → save → run

function TemplatesView({ templates, onSaved, onRun, onError }: {
  templates: PipelineTemplate[];
  onSaved: () => void;
  onRun: () => void;
  onError: (m: string | null) => void;
}) {
  const [picked, setPicked] = useState<string | null>(null);
  const [inputs, setInputs] = useState<Record<string, string>>({});
  const [planned, setPlanned] = useState<PipelineWorkflow | null>(null);
  const [busy, setBusy] = useState(false);
  const [runLocally, setRunLocally] = useState(false);
  const tpl = templates.find((t) => t.id === picked) ?? null;

  const plan = async (t: PipelineTemplate, values: Record<string, string>) => {
    try {
      setPlanned(await pipelinesApi.plan(t.id, values, runLocally ? t.local_routes : undefined));
      onError(null);
    } catch (e) {
      onError(errorMessage(e));
    }
  };

  return (
    <div className="pw-grid">
      <div className="pr-cards">
        {templates.map((t) => (
          <Card className={`pr-card pw-tpl${picked === t.id ? ' on' : ''}`} key={t.id} selected={picked === t.id} label={t.label} onClick={() => { setPicked(t.id); setPlanned(null); setInputs({}); setRunLocally(false); }}>
            <div className="pr-top"><b>{t.label}</b><Badge>{t.steps.length} steps</Badge></div>
            <div className="pr-sub">{t.summary}</div>
            <div className="pr-sub pw-src">{t.source.donor} · {t.source.url.replace('https://github.com/', '')}</div>
          </Card>
        ))}
      </div>
      {tpl ? (
        <div className="pw-detail">
          <h3 className="pr-h">{tpl.label} inputs</h3>
          <p className="pr-sub">{tpl.source.notes}</p>
          <Card className="form">
            {tpl.local_routes ? <Checkbox checked={runLocally} onChange={(v) => { setRunLocally(v); setPlanned(null); }} label="Run locally" /> : null}
            <div className="fgrid pr-row2">
              {tpl.inputs.map((k) => (
                <label className="pw-field" key={k}>
                  <span>{k}</span>
                  {k === 'script' || k === 'dubbed_script' ? (
                    <textarea className="rk-input" value={inputs[k] ?? ''} onChange={(e) => setInputs((v) => ({ ...v, [k]: e.target.value }))} />
                  ) : (
                    <Input value={inputs[k] ?? ''} onChange={(e) => setInputs((v) => ({ ...v, [k]: e.target.value }))} />
                  )}
                </label>
              ))}
            </div>
            <div className="pw-actions">
              <Button type="button" variant="ghost" disabled={busy} onClick={() => void plan(tpl, inputs)}>Plan</Button>
              <Button type="button" variant="ghost" disabled={busy || !planned} onClick={async () => {
                if (!planned) return;
                setBusy(true);
                try {
                  await pipelinesApi.save(planned);
                  onSaved();
                } catch (e) {
                  onError(errorMessage(e));
                } finally {
                  setBusy(false);
                }
              }}>Save workflow</Button>
              <Button type="button" variant="primary" disabled={busy || !planned} onClick={async () => {
                if (!planned) return;
                setBusy(true);
                try {
                  await pipelinesApi.save(planned);
                  await pipelinesApi.runCreate(planned, runLocally ? 0 : undefined, runLocally ? tpl.local_routes : undefined);
                  onRun();
                } catch (e) {
                  onError(errorMessage(e));
                } finally {
                  setBusy(false);
                }
              }}>Save + start run</Button>
            </div>
          </Card>
          {planned ? <StepList steps={planned.steps as unknown as RunStep[]} /> : null}
          {planned?.validation ? (
            <Notice tone={planned.validation.ok ? 'ok' : 'warn'}>
              {planned.validation.ok ? 'Plan is valid.' : planned.validation.errors.join(' ')}
              {planned.validation.warnings.map((w) => <div key={w} className="pr-sub">{w}</div>)}
            </Notice>
          ) : null}
          {planned ? <div className="pr-sub">Estimated wall clock ≈ {Math.round(planned.schedule.total_est_s)}s across {planned.schedule.waves} waves.</div> : null}
        </div>
      ) : (
        <EmptyState title="Pick a donor pipeline to plan a workflow">Steps that {hostAppName()} cannot run natively are marked <b>unsupported</b> up front — nothing is faked.</EmptyState>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// workflows: saved, editable step DAGs

function WorkflowsView({ workflows, onChanged, onRun, onError }: {
  workflows: Array<{ id: string; name: string; template_id: string; steps: number; updated_at?: string }>;
  onChanged: () => void;
  onRun: () => void;
  onError: (m: string | null) => void;
}) {
  const [openId, setOpenId] = useState<string | null>(null);
  const [wf, setWf] = useState<PipelineWorkflow | null>(null);
  const [budget, setBudget] = useState('');

  const open = async (id: string) => {
    setOpenId(id);
    try {
      setWf(await pipelinesApi.get(id));
    } catch (e) {
      onError(errorMessage(e));
    }
  };

  const setStep = (id: string, patch: Partial<RunStep>) =>
    setWf((w) => (w ? { ...w, steps: w.steps.map((s) => (s.id === id ? { ...s, ...patch } : s)) } : w));

  return (
    <div className="pw-grid">
      <Card flush className="group-box pw-list">
        {workflows.length === 0 ? <div className="setting">No saved workflows yet — plan one from Templates.</div> : null}
        {workflows.map((w) => (
          <Card className={`pw-row${openId === w.id ? ' on' : ''}`} key={w.id} selected={openId === w.id} label={w.name} onClick={() => void open(w.id)}>
            <b>{w.name}</b>
            <span className="pr-sub">{w.template_id} · {w.steps} steps {w.updated_at ? `· ${relativeTime(w.updated_at)}` : ''}</span>
          </Card>
        ))}
      </Card>
      {wf ? (
        <div className="pw-detail">
          <div className="pr-top">
            <Input className="pw-name" value={wf.name} onChange={(e) => setWf({ ...wf, name: e.target.value })} />
            <div className="pw-actions">
              <Button type="button" variant="ghost" onClick={async () => {
                try { await pipelinesApi.save(wf); onChanged(); onError(null); } catch (e) { onError(errorMessage(e)); }
              }}>Save</Button>
              <Button type="button" variant="ghost" onClick={async () => {
                try { await pipelinesApi.delete(wf.id); setWf(null); setOpenId(null); onChanged(); } catch (e) { onError(errorMessage(e)); }
              }}>Delete</Button>
              <label className="pw-budget">Budget USD <Input type="number" min="0" step="0.01" value={budget} onChange={(e) => setBudget(e.target.value)} placeholder="uncapped" /></label>
              <Button type="button" variant="primary" onClick={async () => {
                try {
                  await pipelinesApi.save(wf);
                  await pipelinesApi.runCreate(wf, budget ? Number(budget) : undefined);
                  onRun();
                } catch (e) { onError(errorMessage(e)); }
              }}>Start run</Button>
            </div>
          </div>
          <p className="pr-sub pw-src">{wf.source.donor} — {wf.source.notes}</p>
          <StepList steps={wf.steps as unknown as RunStep[]} editable onEdit={setStep} />
        </div>
      ) : (
        <EmptyState title="Select a saved workflow">Edit steps, set a run budget, or start a run.</EmptyState>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// runs: live step DAG + submit/recover/cancel over the job engine

function RunsView({ runs, onChanged, onError }: {
  runs: Array<{ run_id: string; workflow_name: string; status: string; spent_usd: number; budget_usd: number | null; updated_at: string }>;
  onChanged: () => void;
  onError: (m: string | null) => void;
}) {
  const [openId, setOpenId] = useState<string | null>(null);
  const [run, setRun] = useState<PipelineRun | null>(null);
  const [busyStep, setBusyStep] = useState<string | null>(null);
  const poll = useRef<number | null>(null);
  // Steps this window is submitting right now. A multi-job step's per-segment state lives in the engine
  // (run.steps[].fanout), not here; the poll only skips a step while this window is mid-submit.
  const submittingRef = useRef<Set<string>>(new Set());

  const load = useCallback(async (id: string) => {
    try {
      const r = await pipelinesApi.runGet(id);
      setRun(r);
      return r;
    } catch (e) {
      onError(errorMessage(e));
      return null;
    }
  }, [onError]);

  useEffect(() => {
    if (!openId) return;
    void load(openId);
    return () => {
      if (poll.current) window.clearInterval(poll.current);
      poll.current = null;
    };
  }, [openId, load]);

  // Poll running steps and settle them into the run. A multi-job step (the dub step) is read through the
  // engine's `step_status`, which looks at every segment's job and settles the step itself, so the same
  // poll works after a reload: nothing is rebuilt from this window.
  useEffect(() => {
    if (!run) return;
    const fanRunning = run.steps.filter((s) => s.fanout && s.status === 'running');
    const running = run.steps.filter((s) => s.status === 'running' && s.job_id && !s.fanout);
    if ((!running.length && !fanRunning.length) || run.status === 'cancelled') return;
    const timer = window.setInterval(async () => {
      let current = run;
      for (const s of running) {
        try {
          const job = await fetchJobStatus(s.job_id!);
          if (!isTerminalJob(job.status)) continue;
          if (isSuccessJob(job.status)) {
            const result = await fetchJobResult(s.job_id!);
            current = await pipelinesApi.stepSettle(current.run_id, s.id, 'complete', {
              assets: result.assets.map((a) => ({
                asset_id: a.asset_id, path: a.path, sha256: a.sha256, bytes: a.bytes, mime: a.mime,
                kind: s.kind === 'voice' ? 'audio' : s.kind === 'avatar' ? 'video' : a.media, duration_s: a.durationS
              }))
            });
          } else {
            current = await pipelinesApi.stepSettle(current.run_id, s.id, job.status === 'cancelled' ? 'cancelled' : 'failed', {
              error: job.error_message ?? job.error ?? job.status
            });
          }
        } catch (e) {
          onError(errorMessage(e));
        }
      }
      for (const s of fanRunning) {
        if (submittingRef.current.has(s.id)) continue;
        try {
          current = (await pipelinesApi.stepStatus(current.run_id, s.id)).run;
        } catch (e) {
          onError(errorMessage(e));
        }
      }
      setRun(current);
      void onChanged();
    }, 2000);
    poll.current = timer;
    return () => window.clearInterval(timer);
  }, [run, onChanged, onError]);

  const begin = async (step: RunStep) => {
    if (!run) return;
    setBusyStep(step.id);
    try {
      const out = await pipelinesApi.stepBegin(run.run_id, step.id);
      if ((out.executed === 'handoff' || out.executed === 'resume') && out.requests?.length) {
        setRun(out.run);
        await submitSegments(run.run_id, step.id, out.requests);
      } else if (out.executed === 'handoff' && out.request) {
        const job = await submitGeneration({ ...out.request, params: out.request.params });
        const updated = await pipelinesApi.stepAttach(run.run_id, step.id, job.job_id);
        setRun(updated);
      } else {
        setRun(out.run);
      }
      onError(null);
      void onChanged();
    } catch (e) {
      onError(errorMessage(e));
    } finally {
      setBusyStep(null);
    }
  };

  /**
   * Submits the segment requests of a multi-job step as the engine returned them (each carries its
   * deterministic request_id, so submitting one twice finds the same job) and attaches each job to its
   * segment the moment it exists. If a segment is refused, the step is settled failed: the engine
   * cancels the live jobs and keeps the finished clips for a retry of only the rest.
   */
  const submitSegments = async (runId: string, stepId: string, requests: SegmentRequest[]) => {
    submittingRef.current.add(stepId);
    try {
      let latest: PipelineRun | null = null;
      for (const r of requests) {
        let job;
        try {
          job = await submitGeneration(r.request);
        } catch (e) {
          setRun(await pipelinesApi.stepSettle(runId, stepId, 'failed', { error: `segment ${r.segment_id} was refused: ${errorMessage(e)}` }));
          return;
        }
        latest = await pipelinesApi.stepAttach(runId, stepId, job.job_id, r.segment_id);
        setRun(latest);
      }
    } finally {
      submittingRef.current.delete(stepId);
    }
  };

  /** Submit the segments of a begun multi-job step that have no job yet (after a reload or a crash). Same attempt, same request ids. */
  const resumeSegments = async (step: RunStep) => {
    if (!run) return;
    setBusyStep(step.id);
    try {
      const out = await pipelinesApi.stepBegin(run.run_id, step.id);
      setRun(out.run);
      if (out.requests?.length) await submitSegments(run.run_id, step.id, out.requests);
      onError(null);
      void onChanged();
    } catch (e) {
      onError(errorMessage(e));
    } finally {
      setBusyStep(null);
    }
  };

  /** A review choice the user pressed: merge its inputs, retry the held dub_plan, then run it. Nothing auto-applies. */
  const chooseDub = async (step: RunStep, inputs: Record<string, number | boolean>) => {
    if (!run) return;
    setBusyStep(step.id);
    try {
      await pipelinesApi.stepRetry(run.run_id, step.id, inputs);
      const out = await pipelinesApi.stepBegin(run.run_id, step.id);
      setRun(out.run);
      onError(null);
      void onChanged();
    } catch (e) {
      onError(errorMessage(e));
    } finally {
      setBusyStep(null);
    }
  };

  const act = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
      if (openId) await load(openId);
      onError(null);
      void onChanged();
    } catch (e) {
      onError(errorMessage(e));
    }
  };

  const cancel = () =>
    act(async () => {
      if (!run) return;
      const res = await pipelinesApi.runCancel(run.run_id);
      // The engine cancels every job of a multi-job step itself; the ids are listed again here as a belt for single-job steps.
      await Promise.allSettled([...new Set(res.cancel_job_ids)].map((id) => cancelJob(id)));
      setRun(res.run);
    });

  return (
    <div className="pw-grid">
      <Card flush className="group-box pw-list">
        {runs.length === 0 ? <div className="setting">No runs yet — start one from a saved workflow.</div> : null}
        {runs.map((r) => (
          <Card className={`pw-row${openId === r.run_id ? ' on' : ''}`} key={r.run_id} selected={openId === r.run_id} label={r.workflow_name} onClick={() => setOpenId(r.run_id)}>
            <b>{r.workflow_name}</b>
            <span className="pr-sub">
              <Badge tone={r.status === 'succeeded' ? 'ok' : r.status === 'failed' || r.status === 'cancelled' ? 'bad' : r.status === 'running' ? 'accent' : undefined}>{r.status}</Badge>{' '}
              {usd(r.spent_usd)}{r.budget_usd != null ? ` / ${usd(r.budget_usd)}` : ''} · {relativeTime(r.updated_at)}
            </span>
          </Card>
        ))}
      </Card>
      {run ? (
        <div className="pw-detail">
          <div className="pr-top">
            <h3 className="pr-h" style={{ margin: 0 }}>{run.workflow_name} <code className="pr-sub">{run.run_id}</code></h3>
            <div className="pw-actions">
              <Button type="button" variant="ghost" onClick={() => void act(async () => setRun((await pipelinesApi.runRecover(run.run_id)).run))}>Recover</Button>
              <Button type="button" variant="ghost" disabled={run.status === 'cancelled' || run.status === 'succeeded'} onClick={() => void cancel()}>Cancel run</Button>
            </div>
          </div>
          <div className="pr-sub">Spend {usd(run.spent_usd)}{run.budget_usd != null ? ` of ${usd(run.budget_usd)} cap` : ' (uncapped)'}</div>
          <div className="pw-steps">
            {run.steps.map((s) => (
              <RunStepRow key={s.id} step={s} run={run} busy={busyStep === s.id}
                onResume={() => void resumeSegments(s)}
                onChoose={(inputs) => void chooseDub(s, inputs)}
                onBegin={() => void begin(s)}
                onRetry={() => void act(() => pipelinesApi.stepRetry(run.run_id, s.id))}
                onSkip={() => void act(() => pipelinesApi.stepSkip(run.run_id, s.id))} />
            ))}
          </div>
        </div>
      ) : (
        <EmptyState title="Select a run">Watch step state, submit ready steps, cancel or recover.</EmptyState>
      )}
    </div>
  );
}

function RunStepRow({ step, run, busy, onResume, onBegin, onRetry, onSkip, onChoose }: {
  step: RunStep;
  run: PipelineRun;
  busy: boolean;
  onResume: () => void;
  onBegin: () => void;
  onRetry: () => void;
  onSkip: () => void;
  onChoose: (inputs: Record<string, number | boolean>) => void;
}) {
  const reason = blockedReason(step, run);
  const canBegin = step.status === 'ready' && !busy;
  const canRetry = (step.status === 'failed' || step.status === 'cancelled') && step.attempt < step.retry.max_attempts;
  const canSkip = ['pending', 'ready', 'failed', 'unsupported', 'route_unready'].includes(step.status);
  const first = step.assets[0];
  const review: DubReview | null = step.status === 'failed' && first?.review_status === 'needs_review'
    ? dubReviewOf(first, dubMaxTempoOf(run.inputs))
    : null;
  // A multi-job step (the dub step) reads its progress from the engine's per-segment record.
  const fan = step.fanout;
  const live = step.status === 'running' || step.status === 'awaiting_submit';
  const doneSegments = fan ? fan.jobs.filter((j) => j.state === 'complete').length + fan.kept.length : 0;
  const failedSegments = fan ? fan.jobs.filter((j) => j.state === 'failed' || j.state === 'missing').length : 0;
  return (
    <div className={`pw-step st-${step.status}`}>
      <div className="pr-top">
        <b>{step.label}</b>
        <span className="pw-badges">
          <Badge tone={STEP_TONE[step.status]}>{step.status.replace('_', ' ')}</Badge>
          {step.support?.status && step.support.status !== 'ready' && step.support.status !== 'native' ? (
            <Badge tone={SUPPORT_TONE[step.support.status]}>{step.support.status.replace('_', ' ')}</Badge>
          ) : null}
        </span>
      </div>
      <div className="pr-sub">{step.kind} · est {Math.round(step.schedule.est_s)}s{step.budget?.est_usd != null ? ` · ~${usd(step.budget.est_usd)}` : ''}{step.job_id ? ` · job ${step.job_id}` : ''}</div>
      {step.request?.prompt ? <div className="pr-sub pr-clamp">{step.request.prompt}</div> : null}
      {reason ? <Notice tone="warn" className="pr-sub pr-reason">{reason}</Notice> : null}
      {step.error ? <Notice tone="bad" className="pr-sub pr-reason">{step.error}</Notice> : null}
      {fan ? (
        <div className="pr-sub">
          {doneSegments} of {fan.segment_ids.length} segment voice clips complete{fan.kept.length ? ` (${fan.kept.length} kept from an earlier attempt)` : ''}{failedSegments ? `, ${failedSegments} failed` : ''}
        </div>
      ) : null}
      {fan && live && fan.awaits_resolution ? (
        <Notice tone="bad" className="pr-sub pr-reason">
          Job {fan.held.map((h) => `${h.job_id} (segment ${h.segment_id})`).join(', ')} may still be running at the provider. Resolve it (job resolve) before this step can finish or retry.
        </Notice>
      ) : null}
      {fan && live && fan.missing.length ? (
        <Notice tone="warn" className="pr-sub pr-reason" action={<Button type="button" variant="ghost" disabled={busy} onClick={onResume}>{busy ? 'Submitting…' : 'Submit missing segments'}</Button>}>
          Segment {fan.missing.join(', ')} {fan.missing.length === 1 ? 'has' : 'have'} no job yet.
        </Notice>
      ) : null}
      {review ? <DubReviewView review={review} busy={busy} onChoose={onChoose} /> : null}
      {step.native === 'export_srt' ? <div className="pr-sub">Subtitles are aligned per segment, not per word.</div> : null}
      {step.assets.length ? <StepAssets assets={step.assets} /> : null}
      <div className="pw-actions">
        {step.status === 'ready' ? <Button type="button" variant="primary" disabled={!canBegin} onClick={onBegin}>{busy ? 'Submitting…' : step.request ? 'Run step' : 'Execute'}</Button> : null}
        {canRetry ? <Button type="button" variant="ghost" onClick={onRetry}>Retry ({step.attempt}/{step.retry.max_attempts})</Button> : null}
        {canSkip && step.status !== 'skipped' ? <Button type="button" variant="ghost" onClick={onSkip}>Skip</Button> : null}
      </div>
    </div>
  );
}

/** The held dub plan: the per-segment table and the explicit choices. Each choice is a press, never automatic. */
function DubReviewView({ review, busy, onChoose }: {
  review: DubReview;
  busy: boolean;
  onChoose: (inputs: Record<string, number | boolean>) => void;
}) {
  return (
    <div className="pw-review" role="region" aria-label="Retiming review">
      <div className="pr-sub">Some segments do not fit their slot. Nothing is applied until you choose an option.</div>
      <div style={{ overflowX: 'auto' }}>
        <Table dense label="Segments that do not fit their slot">
          <TableHead>
            <TableRow>
              <TableHeaderCell>Segment</TableHeaderCell>
              <TableHeaderCell>Source text</TableHeaderCell>
              <TableHeaderCell>Target text</TableHeaderCell>
              <TableHeaderCell>Slot ms</TableHeaderCell>
              <TableHeaderCell>Synthesised ms</TableHeaderCell>
              <TableHeaderCell>Action</TableHeaderCell>
              <TableHeaderCell>Excess ms</TableHeaderCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {review.rows.map((r) => (
              <TableRow key={r.segment_id}>
                <TableCell>{r.segment_id}</TableCell>
                <TableCell>{r.source_text}</TableCell>
                <TableCell>{r.target_text}</TableCell>
                <TableCell>{r.slot_ms}</TableCell>
                <TableCell>{r.synth_ms}</TableCell>
                <TableCell>{r.action}</TableCell>
                <TableCell>{r.excess_ms}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
      {review.choices.map((c: DubReviewChoice) => (
        <div key={c.id} className="pw-choice" style={{ marginTop: 8 }}>
          {c.inputs ? (
            <Button type="button" variant="ghost" disabled={busy || !c.enabled} onClick={() => onChoose(c.inputs as Record<string, number | boolean>)}>{c.label}</Button>
          ) : (
            <b>{c.label}</b>
          )}
          {c.id === 'allow_shift' ? (
            <div className="pr-sub">{c.drift_ms != null ? `Later segments move up to ${c.drift_ms} ms later (the engine's preview).` : 'The engine reported no drift for this option.'}</div>
          ) : null}
          {c.id === 'shorten_translation' ? (
            <div className="pr-sub">Segments: {(c.segment_ids ?? []).join(', ') || 'none'}. {c.reason}</div>
          ) : c.reason ? <div className="pr-sub pr-reason">{c.reason}</div> : null}
        </div>
      ))}
      {review.notes.map((n) => <div key={n} className="pr-sub">{n}</div>)}
    </div>
  );
}

function StepAssets({ assets }: { assets: RunAsset[] }) {
  return (
    <div className="pw-assets">
      {assets.map((a) => {
        const player = assetKindToPlayer(a.kind, a.mime);
        const src = a.path ? assetUrl(a.path) : '';
        return (
          <div className="pw-asset" key={a.asset_id || a.path}>
            {player && src ? <MediaPlayer src={src} kind={player} title={baseName(a.path)} compact /> : (
              <a className="pw-file" href={src || '#'} target="_blank" rel="noreferrer">{baseName(a.path, a.kind ?? 'artifact')} <span className="pr-sub">{a.mime}</span></a>
            )}
            {a.sha256 ? <div className="pr-sub">sha256 {a.sha256.slice(0, 12)}…</div> : null}
          </div>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// shared step list (plan + edit modes)

function StepList({ steps, editable, onEdit }: {
  steps: RunStep[];
  editable?: boolean;
  onEdit?: (id: string, patch: Partial<RunStep>) => void;
}) {
  const sorted = useMemo(() => [...steps].sort((a, b) => (a.schedule?.wave ?? 0) - (b.schedule?.wave ?? 0)), [steps]);
  return (
    <div className="pw-steps">
      {sorted.map((s) => (
        <div className={`pw-step st-${s.status ?? 'pending'}`} key={s.id}>
          <div className="pr-top">
            <b>{s.label}</b>
            <span className="pw-badges">
              <Badge>w{s.schedule?.wave ?? 0}</Badge>
              {s.support?.status ? <Badge tone={SUPPORT_TONE[s.support.status]}>{s.support.status.replace('_', ' ')}</Badge> : null}
            </span>
          </div>
          <div className="pr-sub">{s.kind} · est {Math.round(s.schedule?.est_s ?? 0)}s{s.depends_on?.length ? ` · after ${s.depends_on.join(', ')}` : ''}</div>
          {s.support?.reason ? <div className="pr-sub pr-reason">{s.support.reason}</div> : null}
          {s.request ? (
            editable && onEdit ? (
              <label className="pw-field">
                <span>prompt</span>
                <textarea className="rk-input" value={s.request.prompt} onChange={(e) => onEdit(s.id, { request: { ...s.request!, prompt: e.target.value } })} />
              </label>
            ) : (
              <div className="pr-sub pr-clamp">{s.request.prompt}</div>
            )
          ) : null}
        </div>
      ))}
    </div>
  );
}

export default PipelineWorkflowsPanel;
