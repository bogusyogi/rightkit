import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import {
  cancelJob,
  cancelLocalImage,
  errorCode,
  errorMessage,
  fetchCredentialStatus,
  fetchDoctor,
  fetchJobList,
  fetchJobResult,
  fetchJobStatus,
  fetchLocalImageProfiles,
  fetchModels,
  fetchSpendStatus,
  getLocalImageJob,
  resolveJob,
  isSuccessJob,
  isHeldJob,
  isTerminalJob,
  submitGeneration,
  submitLocalImage,
  type DoctorReport,
  type GenerateRequest,
  type JobAsset,
  type MediaKind,
  type ModelEntry,
  type SpendStatus
} from './api.js';
import type { LocalImageProfile } from './types.js';

export interface SessionJob {
  key: string;
  source: 'cloud' | 'local';
  jobId: string;
  kind: MediaKind;
  provider: string;
  model: string;
  label: string;
  prompt: string;
  estUsd: number;
  status: string;
  progress: number | null;
  /** Latest engine progress stage: submitted | polling | downloading | complete | failed. */
  stage: string | null;
  stageMessage: string | null;
  error: string | null;
  errorCode: string | null;
  errorMessage: string | null;
  assets: JobAsset[];
  aspect?: string;
  createdAt: number;
}

interface AppState {
  doctor: DoctorReport | null;
  doctorError: string | null;
  models: ModelEntry[];
  modelsError: string | null;
  configured: Set<string>;
  refreshCredentials: () => Promise<void>;
  spend: SpendStatus | null;
  spendError: string | null;
  refreshSpend: () => Promise<void>;
  localProfiles: LocalImageProfile[];
  refreshLocalProfiles: () => Promise<void>;
  jobs: SessionJob[];
  refreshHistory: () => Promise<void>;
  submitCloud: (request: GenerateRequest, label: string, aspect?: string) => Promise<SessionJob>;
  submitLocal: (profile: LocalImageProfile, prompt: string, width: number, height: number) => Promise<SessionJob>;
  cancel: (job: SessionJob) => Promise<void>;
  /** The user's decision for an `outcome_unknown` job (see `job resolve`). */
  resolveHeld: (job: SessionJob, action: 'resubmit' | 'abandon' | 'adopt') => Promise<void>;
}

const Ctx = createContext<AppState | null>(null);

export function useApp(): AppState {
  const value = useContext(Ctx);
  if (!value) throw new Error('useApp outside AppStateProvider');
  return value;
}

const LOCAL_TERMINAL = ['succeeded', 'failed', 'cancelled', 'interrupted'];

/** Nothing left to poll: finished, or (cloud) held as `outcome_unknown` until the user resolves it. */
const isSettled = (job: SessionJob) =>
  job.source === 'local' ? LOCAL_TERMINAL.includes(job.status) : isTerminalJob(job.status) || isHeldJob(job.status);

export function AppStateProvider({ children }: { children: ReactNode }) {
  const [doctor, setDoctor] = useState<DoctorReport | null>(null);
  const [doctorError, setDoctorError] = useState<string | null>(null);
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [configured, setConfigured] = useState<Set<string>>(new Set());
  const [spend, setSpend] = useState<SpendStatus | null>(null);
  const [spendError, setSpendError] = useState<string | null>(null);
  const [localProfiles, setLocalProfiles] = useState<LocalImageProfile[]>([]);
  const [jobs, setJobs] = useState<SessionJob[]>([]);
  const jobsRef = useRef<SessionJob[]>([]);
  jobsRef.current = jobs;

  const refreshCredentials = useCallback(async () => {
    try {
      const status = await fetchCredentialStatus();
      setConfigured(new Set(status.providers.filter((p) => p.configured && p.needs_key !== false).map((p) => p.id)));
    } catch {
      /* engine not ready; Settings shows the error on its own */
    }
  }, []);

  const refreshSpend = useCallback(async () => {
    try {
      setSpend(await fetchSpendStatus());
      setSpendError(null);
    } catch (error) {
      setSpendError(errorMessage(error));
    }
  }, []);

  const refreshLocalProfiles = useCallback(async () => {
    try {
      setLocalProfiles((await fetchLocalImageProfiles()).profiles);
    } catch {
      setLocalProfiles([]);
    }
  }, []);

  // Merge the engine's job history into the session list (results fetched for finished jobs).
  const refreshHistory = useCallback(async () => {
    try {
      const listed = await fetchJobList(100);
      const known = new Set(jobsRef.current.map((j) => j.key));
      const fresh = listed.filter((j) => !known.has(`cloud:${j.job_id}`));
      const added: SessionJob[] = await Promise.all(
        fresh.map(async (j): Promise<SessionJob> => {
          let assets: JobAsset[] = [];
          const ok = isSuccessJob(j.status);
          if (ok) {
            try {
              assets = (await fetchJobResult(j.job_id)).assets ?? [];
            } catch {
              assets = [];
            }
          }
          const done = isTerminalJob(j.status);
          return {
            key: `cloud:${j.job_id}`,
            source: 'cloud',
            jobId: j.job_id,
            kind: (['extend', 'motion_control', 'replace_object', 'restyle'].includes(j.kind ?? '')
              ? 'video'
              : ['image', 'video', 'voice', 'avatar', 'music', 'sfx', 'upscale', 'lipsync'].includes(j.kind ?? '') ? j.kind : 'image') as MediaKind,
            provider: j.provider ?? '',
            model: j.model ?? '',
            label: j.model ?? j.provider ?? 'Job',
            prompt: j.prompt ?? '',
            estUsd: j.est_usd ?? 0,
            status: ok ? 'succeeded' : done ? (j.status.toLowerCase().startsWith('cancel') ? 'cancelled' : 'failed') : j.status,
            progress: null,
            stage: j.progress?.stage ?? null,
            stageMessage: j.progress?.message ?? null,
            error: j.error ?? null,
            errorCode: j.error_code ?? null,
            errorMessage: j.error_message ?? null,
            assets,
            createdAt: j.created_at ? Date.parse(j.created_at) || Date.now() : Date.now()
          };
        })
      );
      if (added.length) {
        setJobs((list) => {
          const have = new Set(list.map((j) => j.key));
          return [...list, ...added.filter((j) => !have.has(j.key))].sort((a, b) => b.createdAt - a.createdAt);
        });
      }
    } catch {
      /* history is best-effort */
    }
  }, []);

  useEffect(() => {
    void refreshHistory();
    fetchDoctor().then((d) => setDoctor(d), (e) => setDoctorError(errorMessage(e)));
    fetchModels().then((m) => setModels(m), (e) => setModelsError(errorMessage(e)));
    void refreshCredentials();
    void refreshSpend();
    void refreshLocalProfiles();
    const timer = window.setInterval(() => void refreshSpend(), 20_000);
    return () => window.clearInterval(timer);
  }, [refreshCredentials, refreshSpend, refreshLocalProfiles, refreshHistory]);

  const patch = useCallback((key: string, next: Partial<SessionJob>) => {
    setJobs((list) => list.map((j) => (j.key === key ? { ...j, ...next } : j)));
  }, []);

  // One poller for every active job: status every 2s until terminal, then fetch the result.
  const pollOne = useCallback(
    async (job: SessionJob) => {
      try {
        if (job.source === 'local') {
          const snap = await getLocalImageJob(job.jobId);
          const state = snap.job.state;
          const done = LOCAL_TERMINAL.includes(state);
          patch(job.key, {
            status: state,
            progress: snap.job.progress,
            error: snap.job.error_message,
            errorMessage: snap.job.error_message,
            assets: done && state === 'succeeded'
              ? snap.artifacts.map((a) => ({ asset_id: a.id, path: a.path, sha256: a.sha256 ?? undefined, mime: 'image/png', media: 'image' }))
              : job.assets
          });
          if (done) void refreshSpend();
          return;
        }
        const status = await fetchJobStatus(job.jobId);
        if (isHeldJob(status.status)) {
          patch(job.key, {
            status: status.status,
            stage: status.progress?.stage ?? null,
            stageMessage: status.progress?.message ?? null,
            error: status.error ?? null,
            errorCode: status.error_code ?? null,
            errorMessage: status.error_message ?? null
          });
          return;
        }
        if (!isTerminalJob(status.status)) {
          patch(job.key, { status: status.status, stage: status.progress?.stage ?? null, stageMessage: status.progress?.message ?? null });
          return;
        }
        if (isSuccessJob(status.status)) {
          const result = await fetchJobResult(job.jobId);
          patch(job.key, { status: 'succeeded', progress: 1, stage: 'complete', stageMessage: null, assets: result.assets ?? [], error: null, errorCode: null, errorMessage: null });
        } else {
          patch(job.key, {
            status: status.status.toLowerCase().startsWith('cancel') ? 'cancelled' : 'failed',
            stage: 'failed',
            error: status.error ?? null,
            errorCode: status.error_code ?? null,
            errorMessage: status.error_message ?? null
          });
        }
        void refreshSpend();
      } catch (error) {
        patch(job.key, { status: 'failed', error: errorMessage(error), errorCode: errorCode(error), errorMessage: errorMessage(error) });
      }
    },
    [patch, refreshSpend]
  );

  const hasActive = jobs.some((j) => !isSettled(j));
  useEffect(() => {
    if (!hasActive) return;
    const tick = () => {
      for (const job of jobsRef.current) {
        if (!isSettled(job)) void pollOne(job);
      }
    };
    const timer = window.setInterval(tick, 2000);
    return () => window.clearInterval(timer);
  }, [hasActive, pollOne]);

  const submitCloud = useCallback(
    async (request: GenerateRequest, label: string, aspect?: string) => {
      const response = await submitGeneration({ ...request, request_id: request.request_id ?? crypto.randomUUID() });
      const job: SessionJob = {
        key: `cloud:${response.job_id}`,
        source: 'cloud',
        jobId: response.job_id,
        kind: request.kind,
        provider: request.provider,
        model: request.model,
        label,
        prompt: request.prompt,
        estUsd: response.est_usd,
        status: response.status || 'queued',
        progress: null,
        stage: 'submitted',
        stageMessage: null,
        error: null,
        errorCode: null,
        errorMessage: null,
        assets: [],
        aspect,
        createdAt: Date.now()
      };
      setJobs((list) => [job, ...list]);
      void refreshSpend();
      return job;
    },
    [refreshSpend]
  );

  const submitLocal = useCallback(async (profile: LocalImageProfile, prompt: string, width: number, height: number) => {
    const { job: created } = await submitLocalImage({
      request_id: crypto.randomUUID(),
      profile_id: profile.id,
      prompt,
      width,
      height,
      reference_ids: []
    });
    const job: SessionJob = {
      key: `local:${created.id}`,
      source: 'local',
      jobId: created.id,
      kind: 'image',
      provider: 'local',
      model: profile.id,
      label: profile.label,
      prompt,
      estUsd: 0,
      status: created.state,
      progress: created.progress,
      stage: null,
      stageMessage: null,
      error: null,
      errorCode: null,
      errorMessage: null,
      assets: [],
      aspect: `${width}x${height}`,
      createdAt: Date.now()
    };
    setJobs((list) => [job, ...list]);
    return job;
  }, []);

  const cancel = useCallback(
    async (job: SessionJob) => {
      try {
        if (job.source === 'local') await cancelLocalImage(job.jobId);
        else await cancelJob(job.jobId);
        patch(job.key, { status: 'cancelled' });
        void refreshSpend();
      } catch (error) {
        patch(job.key, { error: errorMessage(error), errorMessage: errorMessage(error), errorCode: errorCode(error) });
      }
    },
    [patch, refreshSpend]
  );

  const resolveHeld = useCallback(
    async (job: SessionJob, action: 'resubmit' | 'abandon' | 'adopt') => {
      await resolveJob(job.jobId, action);
      if (action === 'resubmit') {
        patch(job.key, { status: 'submitted', stage: 'submitted', stageMessage: null, error: null, errorCode: null, errorMessage: null });
      } else if (action === 'adopt') {
        // Complete in the engine: the next status poll loads the stored result.
        patch(job.key, { status: 'running', stage: 'downloading', stageMessage: null, error: null, errorCode: null, errorMessage: null });
      } else {
        patch(job.key, {
          status: 'failed',
          stage: 'failed',
          error: 'outcome_unknown_abandoned: abandoned by you; the provider may have charged for it',
          errorCode: 'outcome_unknown_abandoned',
          errorMessage: 'Abandoned by you; the provider may have charged for it.'
        });
      }
      void refreshSpend();
    },
    [patch, refreshSpend]
  );

  const value = useMemo<AppState>(
    () => ({
      doctor, doctorError, models, modelsError, configured, refreshCredentials, spend, spendError, refreshSpend,
      localProfiles, refreshLocalProfiles, jobs, refreshHistory, submitCloud, submitLocal, cancel, resolveHeld
    }),
    [doctor, doctorError, models, modelsError, configured, refreshCredentials, spend, spendError, refreshSpend, localProfiles, refreshLocalProfiles, jobs, refreshHistory, submitCloud, submitLocal, cancel, resolveHeld]
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export const jobIsActive = (job: SessionJob) => !isSettled(job);
