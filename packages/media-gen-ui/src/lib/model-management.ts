// Model-management helpers shared by the Models panel and its tests. Pure
// logic only — engine calls live in `ModelsPanel.tsx` via `studio()`.
// Contract: `studio.models.*` (crates/rightkit-media-gen-models/src/lib.rs,
// docs/MODEL-MANAGEMENT.md).

export type EngineKind =
  | 'native-bundled'
  | 'rented-remote'
  | 'experimental'
  | string;

export type ReadinessState =
  | 'native_ready'
  | 'missing'
  | 'api'
  | 'rental'
  | 'unsupported_backend'
  | 'unqualified'
  | string;

export type FitState =
  | 'measured'
  | 'estimated'
  | 'unqualified'
  | 'unsupported'
  | 'rental_only'
  | 'api'
  | string;

export interface HardwareGpu {
  name: string;
  vram_gb?: number | null;
  unified?: boolean;
  backends?: string[];
}

export interface HardwareProfile {
  os?: string;
  arch?: string;
  memory_gb?: number | null;
  gpus?: HardwareGpu[];
  profile?: string;
  source?: 'detected' | 'override' | string;
}

export interface ModelEntry {
  id: string;
  label?: string;
  modality?: string;
  engine?: string;
  engine_kind?: EngineKind;
  revision?: string | number;
  quantization?: string | null;
  backends?: string[];
  capabilities?: string[];
  requirements?: { memory_gb?: number | null; vram_gb?: number | null; cuda_vram_gb?: number | null; mps_memory_gb?: number | null; disk_gb?: number | null };
  license?: { name?: string; url?: string; commercial?: boolean };
  measured?: Array<{ profile: string; peak_gb?: number; warm_rtf?: number; notes?: string }>;
  installable?: boolean;
  readiness?: { state: ReadinessState; missing_files?: string[]; worker_installed?: boolean; configured?: boolean; detail?: string };
  fit?: { fit: FitState; required_gb?: number; available_gb?: number; reasons?: string[]; evidence?: unknown };
}

export type WorkerState =
  | 'configured' | 'staging' | 'staged' | 'running' | 'idle'
  | 'retrieving' | 'retrieved' | 'terminated' | 'error' | string;

export interface RentedWorker {
  worker_id: string;
  label?: string;
  spec?: Record<string, unknown>;
  driver?: string;
  state: WorkerState;
  budget_usd?: number;
  accrued_usd?: number;
  idle_since?: string | null;
  idle_limit_min?: number | null;
  outputs?: Array<{ file: string; path?: string; bytes?: number; sha256?: string; hash_verified?: boolean }>;
  events?: Array<{ at: string; action: string; from: string | null; to: string; detail?: unknown }>;
}

export const WORKER_ACTIONS = ['register', 'provision', 'stage', 'reconnect', 'begin', 'retrieve', 'idle', 'unload', 'teardown', 'status'] as const;
export type WorkerAction = (typeof WORKER_ACTIONS)[number];

// ---------- installs (studio.models.install.*) ----------------------------------

export type InstallState =
  | 'new' | 'installing' | 'pausing' | 'paused' | 'cancelling' | 'cancelled' | 'installed'
  | 'failed' | 'uninstalled' | string;

export interface InstallRecord {
  id: string;
  state: InstallState;
  revision?: string;
  dest?: string;
  files?: Array<{ name: string; bytes?: number | null; sha256?: string | null; state?: string }>;
  events?: Array<{ at: string; action: string; detail?: unknown }>;
}

export interface InstallProgress {
  bytes_done?: number;
  bytes_total?: number;
  file?: string | null;
}

export function installStateLabel(rec: InstallRecord | null | undefined): string {
  switch (rec?.state) {
    case 'installing': return 'Installing';
    case 'pausing': return 'Pausing';
    case 'paused': return 'Paused (resumable)';
    case 'cancelling': return 'Cancelling';
    case 'cancelled': return 'Cancelled';
    case 'installed': return `Installed ${rec.revision ?? ''}`.trim();
    case 'failed': return 'Install failed';
    case 'uninstalled': return 'Uninstalled';
    case 'new': return 'Queued';
    default: return rec?.state ?? 'Not installed';
  }
}

/** 0–100 progress; null when no download is active or total is unknown. */
export function installProgressPct(p: InstallProgress | null | undefined): number | null {
  if (!p || typeof p.bytes_done !== 'number' || !p.bytes_total || p.bytes_total <= 0) return null;
  return Math.min(100, Math.round((p.bytes_done / p.bytes_total) * 100));
}

export function installProgressText(p: InstallProgress | null | undefined): string {
  const pct = installProgressPct(p);
  const file = p?.file ? ` — ${p.file}` : '';
  return pct == null ? `Downloading${file}` : `${pct}%${file}`;
}

/** Per-model install actions for its current state. */
export function installActions(entry: ModelEntry, rec: InstallRecord | undefined): Array<'install' | 'pause' | 'cancel' | 'upgrade' | 'uninstall' | 'rollback'> {
  const out: Array<'install' | 'pause' | 'cancel' | 'upgrade' | 'uninstall' | 'rollback'> = [];
  const live = rec?.state === 'installing' || rec?.state === 'pausing' || rec?.state === 'cancelling';
  if (live) {
    if (rec?.state === 'installing') out.push('pause', 'cancel');
    return out;
  }
  const ready = entry.readiness?.state === 'native_ready';
  const installable = entry.installable === true;
  if (installable && (!ready || rec?.state === 'failed' || rec?.state === 'cancelled' || rec?.state === 'paused')) out.push('install');
  if (installable && rec?.state === 'paused') out.push('cancel');
  // No 'rollback': the model store keeps one revision per model (rightkit-models 0.7), so there is
  // never an earlier revision to go back to and the command only answers `no_rollback`. The action
  // stays in the type for hosts that still name it.
  if (installable && ready) out.push('upgrade', 'uninstall');
  else if (installable && rec?.state === 'installed') {
    out.push('uninstall');
  }
  return out;
}

// ---------- readiness / fit presentation -------------------------------------

const READINESS_LABEL: Record<string, string> = {
  native_ready: 'Ready (native)',
  missing: 'Not installed',
  api: 'Provider API',
  rental: 'Rental route',
  unsupported_backend: 'Unsupported backend',
  unqualified: 'Unqualified',
};

const READINESS_TONE: Record<string, 'ok' | 'warn' | 'bad' | 'local' | undefined> = {
  native_ready: 'ok',
  missing: 'warn',
  api: 'local',
  rental: 'local',
  unsupported_backend: 'bad',
  unqualified: undefined,
};

export function readinessLabel(entry: ModelEntry): string {
  const state = entry.readiness?.state ?? 'unqualified';
  if (state === 'missing' && entry.readiness?.missing_files?.length) {
    return `Not installed (${entry.readiness.missing_files.length} files)`;
  }
  return READINESS_LABEL[state] ?? state;
}

export function readinessTone(entry: ModelEntry): 'ok' | 'warn' | 'bad' | 'local' | undefined {
  return READINESS_TONE[entry.readiness?.state ?? ''] ?? undefined;
}

const FIT_LABEL: Record<string, string> = {
  measured: 'Measured here',
  estimated: 'Estimated compatible',
  unqualified: 'Unqualified',
  unsupported: "Doesn't fit detected hardware",
  rental_only: 'Rental/API only',
  api: 'Provider route',
};

const FIT_TONE: Record<string, 'ok' | 'warn' | 'bad' | undefined> = {
  measured: 'ok',
  estimated: 'ok',
  unqualified: 'warn',
  unsupported: 'bad',
  rental_only: 'warn',
};

export function fitLabel(entry: ModelEntry): string {
  const f = entry.fit?.fit ?? 'unqualified';
  const base = FIT_LABEL[f] ?? f;
  const req = entry.fit?.required_gb;
  return typeof req === 'number' ? `${base} (~${Math.round(req)} GB)` : base;
}

export function fitTone(entry: ModelEntry): 'ok' | 'warn' | 'bad' | undefined {
  return FIT_TONE[entry.fit?.fit ?? ''] ?? undefined;
}

/** Why an action button is disabled; null when it can run. */
export function actionBlocker(action: 'probe' | 'install' | 'unload', entry: ModelEntry): string | null {
  const state = entry.readiness?.state;
  const engine = entry.engine;
  if (action === 'install') {
    if (state === 'native_ready') return 'Already installed';
    if (engine === 'rented') return 'No local install — stage a rented worker';
    return null;
  }
  if (action === 'unload') {
    if (engine === 'rented') return 'Use worker teardown below';
    if (state !== 'native_ready') return 'Nothing running';
    return null;
  }
  return null;
}

// ---------- hardware -----------------------------------------------------------

export function hardwareSummary(hw: HardwareProfile | null | undefined): string {
  if (!hw) return 'Unknown hardware';
  const gpu = hw.gpus?.[0]?.name;
  const mem = typeof hw.memory_gb === 'number' ? `${Math.round(hw.memory_gb)} GB` : 'unknown memory';
  const parts = [hw.os ?? 'unknown os', hw.arch ?? '', mem, gpu ?? 'no GPU detected'].filter(Boolean);
  return parts.join(' · ');
}

/** Which requirement number applies to this host (mirrors the Rust probe). */
export function requiredGbFor(entry: ModelEntry, hw: HardwareProfile | null | undefined): number | null {
  const req = entry.requirements ?? {};
  const gpu = hw?.gpus?.[0];
  if (gpu?.unified) return req.mps_memory_gb ?? req.memory_gb ?? req.vram_gb ?? null;
  if (gpu && typeof gpu.vram_gb === 'number' && gpu.vram_gb > 0) return req.cuda_vram_gb ?? req.vram_gb ?? req.memory_gb ?? null;
  return req.memory_gb ?? req.vram_gb ?? null;
}

// ---------- catalog -----------------------------------------------------------

const MODALITY_ORDER = ['image', 'voice', 'video', 'avatar', 'music', 'sfx', 'upscale'];

export function groupByModality(entries: ModelEntry[]): Array<{ modality: string; entries: ModelEntry[] }> {
  const groups = new Map<string, ModelEntry[]>();
  for (const e of entries) {
    const key = e.modality ?? 'other';
    groups.set(key, [...(groups.get(key) ?? []), e]);
  }
  return [...groups.entries()].sort(([a], [b]) => {
    const ia = MODALITY_ORDER.indexOf(a);
    const ib = MODALITY_ORDER.indexOf(b);
    return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
  }).map(([modality, list]) => ({ modality, entries: list }));
}

// ---------- rented workers ------------------------------------------------------

const WORKER_STATE_LABEL: Record<string, string> = {
  configured: 'Configured',
  staging: 'Staging',
  staged: 'Staged',
  running: 'Running',
  idle: 'Idle',
  retrieving: 'Retrieving',
  retrieved: 'Retrieved',
  terminated: 'Terminated',
  error: 'Error',
};

export function workerStateLabel(w: RentedWorker): string {
  return WORKER_STATE_LABEL[w.state] ?? w.state;
}

export function workerStateTone(w: RentedWorker): 'ok' | 'warn' | 'bad' | 'run' | undefined {
  switch (w.state) {
    case 'running': return 'run';
    case 'staged': case 'retrieved': return 'ok';
    case 'idle': case 'configured': case 'staging': case 'retrieving': return 'warn';
    case 'error': return 'bad';
    default: return undefined;
  }
}

/** Actions valid from the worker's current state (mirrors the Rust transitions). */
export function workerActions(w: RentedWorker): WorkerAction[] {
  const vast = w.spec?.driver === 'vast';
  const base = (a: WorkerAction[]): WorkerAction[] => a;
  switch (w.state) {
    case 'configured': return base(vast ? ['provision', 'stage', 'teardown'] : ['stage', 'teardown']);
    case 'error': return base(vast ? ['provision', 'stage', 'teardown'] : ['stage', 'teardown']);
    case 'staged': return ['begin', 'reconnect', 'retrieve', 'teardown'];
    case 'running': return ['idle', 'reconnect', 'retrieve', 'teardown'];
    case 'idle': return ['begin', 'retrieve', 'unload', 'teardown'];
    case 'retrieved': return ['unload', 'teardown'];
    case 'terminated': return [];
    default: return [];
  }
}

export function driverLabel(w: RentedWorker): string {
  const d = (w.spec?.driver as string | undefined) ?? w.driver ?? 'none';
  return d;
}

/** True when this worker can only be created via the gated provision action. */
export function needsProvision(w: RentedWorker): boolean {
  return w.spec?.driver === 'vast' && !((w.spec?.vast as Record<string, unknown> | undefined)?.instance_id);
}

export function budgetSummary(w: RentedWorker): string {
  const budget = w.budget_usd ?? 0;
  const accrued = w.accrued_usd ?? 0;
  return budget > 0 ? `$${accrued.toFixed(2)} of $${budget.toFixed(2)} budget` : `$${accrued.toFixed(2)} accrued (no cap)`;
}

// ---------- memory governor (studio.models.governor.*) -------------------------

export interface LoadedModelInfo {
  runtime: string;
  model_id: string;
  rss_gb?: number | null;
  idle?: boolean;
}

export interface GovernorGrant {
  id: string;
  job?: string;
  need_gb?: number;
  at?: string;
}

export interface GovernorStatus {
  loaded?: LoadedModelInfo[];
  ram?: { free_gb?: number; resident_gb?: number; reserve_gb?: number };
  grants?: GovernorGrant[];
  events?: Array<{ at: string; action: string; detail?: unknown }>;
}

export function gbLabel(gb: number | null | undefined): string {
  return typeof gb === 'number' && Number.isFinite(gb) ? `${gb.toFixed(1)} GB` : 'unknown';
}

export function revisionLabel(entry: ModelEntry): string {
  const rev = entry.revision == null ? '' : typeof entry.revision === 'number' ? `r${entry.revision}` : String(entry.revision);
  const quant = entry.quantization ?? '';
  return [rev, quant].filter(Boolean).join(' · ');
}
