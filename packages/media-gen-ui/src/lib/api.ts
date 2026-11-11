import { mediaGenClient } from './client.js';

export { mediaGenClient, setMediaGenClient, tauriClient, type MediaGenClient } from './client.js';

/** True when the host's client reaches an engine on this machine (desktop-only features). */
export const isTauri = (): boolean => mediaGenClient().isDesktop();
import { toError } from './engine-error.js';
import { priceFactsFromModalities, type ModelFacts, type ModelPriceFacts } from './gen-params.js';
export type { ModelPriceFacts } from './gen-params.js';
import type { RentalsEnforceReport, RentalsListing, RentalWorkerRecord } from '../views/rentals-logic.js';
import type { AssetImportDryRun, AssetImportRequest, AssetImportResult, LibrarySearchRequest, LibrarySearchResult } from './types/library.js';
import type {
  GalleryResponse,
  LocalImageJob,
  LocalImageJobSnapshot,
  LocalImageListResponse,
  LocalImageProfile,
  LocalImageReference,
  LocalImageRuntimeStatus,
  LocalImageSubmitRequest
} from './types.js';
export { EngineError, errorCode, errorDetails, errorMessage, errorText, toError } from './engine-error.js';
export type { EngineErrorDetails } from './engine-error.js';


export type MediaKind = 'image' | 'video' | 'voice' | 'avatar' | 'music' | 'sfx' | 'upscale' | 'lipsync';

export interface ModelEntry extends ModelFacts {
  provider: string;
  model: string;
  /** `avatar` models exist in the engine catalog but have no workspace yet. */
  kind: MediaKind;
  label: string;
  est_usd: number;
  durations_s?: number[] | null;
  min_duration_s?: number | null;
  max_duration_s?: number | null;
  default_duration_s?: number | null;
  /**
   * Reference images the model's adapter reads (image models only). Absent when it takes none;
   * `max` is absent when the adapter validates no count. The composer falls back to its own cap then.
   */
  references?: { param: string; max?: number; local_upload?: 'fal_storage' | 'none' } | null;
}

export interface CredentialRuntime {
  ready: boolean;
  /** True for an installed external scripted worker (e.g. Python venv) — a supported route, never bundled/native readiness. */
  external?: boolean;
  code?: string;
  message?: string;
}

export interface CredentialProvider {
  id: string;
  configured: boolean;
  source?: 'keychain' | 'environment' | 'local' | null;
  /** False for local engines (local-qts): no API key required. */
  needs_key?: boolean;
  /** Keyless providers only: whether the local runtime is actually installed. Absent for keyed providers. */
  runtime?: CredentialRuntime | null;
}

export interface CredentialStatus {
  providers: CredentialProvider[];
}

export interface ImportEnvResult {
  imported: string[];
  skipped_present: string[];
  missing: string[];
  source: string;
}

/**
 * One period's four amounts from `spend.status` (`amounts.day` / `amounts.month`), plus the
 * settled and cap-counted figures beside them. Each amount stays separate; nothing is merged.
 * `estimated_usd`, `provider_reported_usd` and `invoice_reconciled_usd` are `null` when no row supplies them.
 */
export interface SpendPeriodAmounts {
  rows: number;
  /** The engine's estimate at submit, over the rows that have a job. `null` with no job rows. */
  estimated_usd: number | null;
  /** The ledger's reserved column: the reservations made, open or settled. */
  reserved_usd: number;
  /** What the provider's response stated. `null` until a provider reports one. */
  provider_reported_usd: number | null;
  provider_reported_rows: number;
  provider_reported_available: boolean;
  /** The user's invoice lines, recorded by `spend.reconcile`. `null` until a row is reconciled. */
  invoice_reconciled_usd: number | null;
  invoice_reconciled_rows: number;
  /** Sum of settled `actual_usd`. */
  settled_usd: number;
  /** Reservations still open. */
  open_reserved_usd: number;
  /** Exactly what the caps counted for these rows: `settled_usd + open_reserved_usd`. */
  cap_counted_usd: number;
}

export interface SpendAmountsByPeriod {
  day: SpendPeriodAmounts;
  month: SpendPeriodAmounts;
}

export interface SpendStatus {
  day: string;
  cap_usd: number;
  reserved_usd: number;
  actual_usd: number;
  /** UTC calendar month, `YYYY-MM`. */
  month: string;
  /** `null` when no monthly cap is set. */
  monthly_cap_usd: number | null;
  month_reserved_usd: number;
  month_actual_usd: number;
  month_counted_usd: number;
  monthly_remaining_usd: number | null;
  /** The four separate amounts per period. The caps count none of them except through `reserved_usd` / `actual_usd`. */
  amounts: SpendAmountsByPeriod;
}

/** `YYYY-MM` (a whole UTC month) or an inclusive UTC `{from, to}` range of `YYYY-MM-DD` days. */
export type SpendInvoicePeriod = string | { from: string; to: string };

/** One invoice line. Exactly one of `job_id` / `provider_job_id`; `attempt` only when a job has several rows. */
export type SpendInvoiceLine = {
  job_id?: string;
  provider_job_id?: string;
  attempt?: number;
  usd: number;
};

/** `spend.reconcile` input. Writes only the invoice record and the matched rows' invoice amounts; never a cap or reservation. */
export type SpendReconcileInput = {
  period: SpendInvoicePeriod;
  provider: string;
  invoice_total_usd: number;
  invoice_ref?: string;
  lines?: SpendInvoiceLine[];
};

export interface SpendReconcileMatchedLine {
  job_id: string;
  attempt: number;
  invoice_usd: number;
  settled_usd: number;
  difference_usd: number;
}

/** A line the engine could not match. `reason` is an engine reason string, e.g. `no_ledger_row_for_provider_and_period`. */
export interface SpendReconcileUnmatchedLine {
  index: number;
  job_id: string | null;
  provider_job_id: string | null;
  attempt: number | null;
  usd: number;
  reason: string;
}

/** A ledger row in the period with no invoice line. `settled_usd` is `null` while the reservation is open. */
export interface SpendReconcileUnmatchedRow {
  job_id: string;
  attempt: number;
  settled_usd: number | null;
  reserved_usd: number;
}

export interface SpendReconcileDifferences {
  /** Invoice total against everything the engine settled for the provider and period. */
  invoice_total_minus_settled_usd: number;
  settled_minus_estimated_usd: number | null;
  provider_reported_minus_settled_usd: number | null;
  invoice_reconciled_minus_settled_usd: number | null;
  /** Lines total against the invoice total. `null` when no lines were sent. */
  lines_total_minus_invoice_total_usd: number | null;
}

/** `spend.reconcile` result: the period's amounts for the provider, plus what the invoice matched. */
export interface SpendReconcileResult extends SpendPeriodAmounts {
  provider: string;
  period: { from: string; to: string };
  invoice_ref: string;
  invoice_ref_generated: boolean;
  /** True when this invoice reference was already recorded with the same provider, period and total. */
  already_recorded: boolean;
  invoice_total_usd: number;
  differences: SpendReconcileDifferences;
  /** Present only when lines were sent. */
  lines?: {
    given: number;
    matched: SpendReconcileMatchedLine[];
    unmatched_lines: SpendReconcileUnmatchedLine[];
    unmatched_rows: SpendReconcileUnmatchedRow[];
    lines_total_usd: number;
  };
}

export interface GenerateRequest {
  kind: MediaKind;
  provider: string;
  model: string;
  prompt: string;
  params?: Record<string, unknown>;
  request_id?: string;
  /**
   * Per-run approval to upload local reference images to the person's own fal storage.
   * Send `true` only for the run the person just confirmed; the engine refuses a local
   * NanoBanana reference with `upload_confirmation_required` without it.
   */
  confirm_fal_upload?: boolean;
}

/** Latest `job.progress` event from the engine. */
export interface JobProgress {
  stage?: 'submitted' | 'polling' | 'downloading' | 'complete' | 'failed' | string;
  status?: string;
  attempt?: number;
  message?: string | null;
  at?: string;
}

export interface JobStatus {
  job_id: string;
  status: string;
  progress?: JobProgress | null;
  error_code?: string | null;
  error_message?: string | null;
  kind?: string;
  provider?: string;
  model?: string;
  prompt?: string;
  est_usd?: number;
  created_at?: string;
  error?: string | null;
}

export interface JobAsset {
  asset_id: string;
  path: string;
  sha256?: string;
  bytes?: number;
  mime?: string;
  /** Media kind: image | video | voice (audio). */
  media?: string;
  width?: number | null;
  height?: number | null;
  durationS?: number | null;
}

export interface JobResult {
  job_id?: string;
  assets: JobAsset[];
}

export type DoctorReport = Record<string, unknown>;

export interface InstalledApp {
  id?: string;
  name: string;
  version?: string;
  path?: string;
}

async function engine<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  try {
    return await mediaGenClient().request<T>(method, params);
  } catch (error) {
    throw toError(error);
  }
}

export type ApprovalKind = 'recipes' | 'video_templates' | 'recipe_templates' | 'character_training' | 'eyes' | 'blender_finish';

/**
 * Approve a preflight draft as the user. Call it only from the approve button's click handler: the
 * Tauri command `approve_draft` mints the engine's approval token with a secret that lives in the
 * app process only, so no engine command (`studio.*.approve`) approves on its own. `displayedHash`
 * is the draft's `request_hash` (recipes, recipe templates and batch items) or `fingerprint` (video templates) as the user saw it.
 * `character_training` is a paid fal spend: `draftId` and `displayedHash` are the preflight `plan_id`, and
 * `params` is the train request the plan was made from (the engine refuses any other plan); it returns the
 * started training. `eyes` (the human-eyes approval that releases an asset for delivery) takes `params` too: `{path}` or
 * `{asset_id}`, with `draftId` the hex of the asset's content hash and `displayedHash` that hash (`sha256:...`) from
 * `studio.eyes.status`. `blender_finish` (the owner's confirmation of a Blender product spec and its finishes) takes
 * `{path}` (the spec file) with `draftId` and `displayedHash` both the `draft_id` from `studio.blender.spec.preflight`; the engine
 * then writes the confirmation into the spec. No other kind takes `params`.
 */
export async function approveDraft<T>(kind: ApprovalKind, draftId: string, displayedHash: string, params?: Record<string, unknown>): Promise<T> {
  try {
    return await mediaGenClient().approveDraft<T>({ kind, draftId, displayedHash, ...(params ? { params } : {}) });
  } catch (error) {
    throw toError(error);
  }
}

/** Call a `studio.<area>.<verb>` engine command (the studio crates: rightkit-prompt-recipes, rightkit-media-gen-models, ...). */
export function studio<T>(name: string, input: Record<string, unknown> = {}): Promise<T> {
  return command<T>(name.startsWith('studio.') ? name : `studio.${name}`, input);
}

function command<T>(name: string, input: Record<string, unknown> = {}): Promise<T> {
  return engine<T>('command', { name, input });
}

/** Run a named engine command (used by the licence client, which stores through the engine). */
export const engineCommand = command;

/** Saves a PNG (e.g. an inpaint mask) under app data through the engine and returns its local path. */
export async function saveReferencePng(png: Blob, name: string): Promise<string> {
  const bytes = new Uint8Array(await png.arrayBuffer());
  let binary = '';
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  const res = await command<{ path: string }>('reference.save_png', { png_base64: btoa(binary), name });
  return res.path;
}

/** A paid submit that may have reached the provider. Not terminal, but nothing drives it: the user resolves it. */
export const OUTCOME_UNKNOWN = 'outcome_unknown';
export const isHeldJob = (status: string) => status.toLowerCase() === OUTCOME_UNKNOWN;
export const resolveJob = (jobId: string, action: 'resubmit' | 'abandon' | 'adopt') =>
  command<unknown>('job.resolve', { job_id: jobId, action });

export const TERMINAL_JOB_STATES = ['succeeded', 'completed', 'complete', 'done', 'failed', 'error', 'cancelled', 'canceled'];
export const isTerminalJob = (status: string) => TERMINAL_JOB_STATES.includes(status.toLowerCase());
export const isSuccessJob = (status: string) => ['succeeded', 'completed', 'complete', 'done'].includes(status.toLowerCase());

function listOf<T>(value: unknown, key: string): T[] {
  if (Array.isArray(value)) return value as T[];
  if (value && typeof value === 'object') {
    const inner = (value as Record<string, unknown>)[key];
    if (Array.isArray(inner)) return inner as T[];
  }
  return [];
}

/** Optional server-side filters; an omitted field matches every artifact. */
export interface GalleryIndexFilters {
  project?: string;
  /** Only artifacts with no project. Exclusive with `project`; the engine refuses both together. */
  no_project?: boolean;
  brand?: string;
  eyes?: string;
  jury?: string;
  /** Only files registered by this engine caller (slug, e.g. `postright`). */
  caller?: string;
}

export async function fetchGallery(params: { roots?: string[]; offset?: number; limit?: number; limitPerRoot?: number; includeIntermediate?: boolean } & GalleryIndexFilters = {}): Promise<GalleryResponse> {
  return engine<GalleryResponse>('index_artifacts', {
    roots: params.roots ?? [],
    offset: params.offset,
    limit: params.limit,
    limitPerRoot: params.limitPerRoot,
    includeIntermediate: Boolean(params.includeIntermediate),
    project: params.project,
    no_project: params.no_project,
    brand: params.brand,
    eyes: params.eyes,
    jury: params.jury,
    caller: params.caller
  });
}

export async function fetchEngineSettings(): Promise<{ gallery_roots?: string[]; output_root?: string }> {
  return engine('get_settings');
}

export const fetchDoctor = () => command<DoctorReport>('doctor');
export async function fetchApps(): Promise<InstalledApp[]> {
  // Engine shape: {app, app_path, engine_path, version, source}.
  return listOf<Record<string, string | undefined>>(await command<unknown>('apps'), 'apps').map((a) => ({
    id: a.id ?? a.app,
    name: a.name ?? a.app ?? 'Unknown app',
    version: a.version,
    path: a.path ?? a.app_path
  }));
}
export async function fetchModels(): Promise<ModelEntry[]> {
  return listOf<ModelEntry>(await command<unknown>('models'), 'models');
}
/**
 * Price facts per `provider/model` from `media modalities`. The `models` rows do not carry them: the engine
 * states `reconciled` and the rounding of a per-megapixel price only in the modalities answer.
 */
export async function fetchModelPriceFacts(): Promise<Record<string, ModelPriceFacts>> {
  return priceFactsFromModalities(await command<unknown>('media.modalities'));
}
export const fetchCredentialStatus = () => command<CredentialStatus>('credentials.status');
export const importCredentialsFromEnv = (overwrite = false) =>
  command<ImportEnvResult>('credentials.import_env', { overwrite });
export const setCredential = (provider: string, apiKey: string) =>
  command<unknown>('credentials.set', { provider, api_key: apiKey });
export const testCredential = (provider: string) =>
  command<{ ok: boolean; message: string }>('credentials.test', { provider });
export const submitGeneration = (request: GenerateRequest) =>
  command<{ job_id: string; status: string; est_usd: number }>('generate', { ...request });
/** The engine's own reserved estimate for these exact params: `generate` with `dry_run`, which stores and sends nothing. */
export const estimateGeneration = (request: GenerateRequest) =>
  command<{ dry_run: boolean; est_usd: number }>('generate', { ...request, dry_run: true });
/** Engine job records keep the prompt under `request` and errors as `{code, message}`. */
function normalizeJob(raw: unknown): JobStatus {
  const job = { ...(raw as Record<string, unknown>) };
  const request = job.request as { prompt?: string } | undefined;
  if (job.prompt == null && request?.prompt) job.prompt = request.prompt;
  const error = job.error as { code?: string; message?: string } | string | null | undefined;
  if (error && typeof error === 'object') {
    job.error_code = error.code ?? null;
    job.error_message = error.message ?? null;
    job.error = error.code ? `${error.code}: ${error.message ?? ''}` : (error.message ?? null);
  } else if (typeof error === 'string') {
    job.error_message = error;
  }
  const progress = job.progress;
  job.progress = progress && typeof progress === 'object' ? progress : null;
  return job as unknown as JobStatus;
}

export const fetchJobStatus = async (jobId: string) => normalizeJob(await command<unknown>('job.status', { job_id: jobId }));
/** Engine descriptors carry `media` as `{kind, width, height, duration_s}`; flatten for the UI. */
function normalizeResult(raw: unknown): JobResult {
  const result = (raw ?? {}) as { job_id?: string; assets?: Array<Record<string, unknown>> };
  return {
    job_id: result.job_id,
    assets: (result.assets ?? []).map((a) => {
      const media = a.media as string | { kind?: string; width?: number | null; height?: number | null; duration_s?: number | null } | undefined;
      const info = media && typeof media === 'object' ? media : null;
      return {
        asset_id: String(a.asset_id ?? ''),
        path: String(a.path ?? ''),
        sha256: a.sha256 as string | undefined,
        bytes: typeof a.bytes === 'number' ? a.bytes : undefined,
        mime: a.mime as string | undefined,
        media: info ? info.kind : (media as string | undefined),
        width: info?.width ?? null,
        height: info?.height ?? null,
        durationS: info?.duration_s ?? null
      };
    })
  };
}

export const fetchJobResult = async (jobId: string) => normalizeResult(await command<unknown>('job.result', { job_id: jobId }));
export async function fetchJobList(limit = 50): Promise<JobStatus[]> {
  return listOf<unknown>(await command<unknown>('job.list', { limit }), 'jobs').map(normalizeJob);
}
export const cancelJob = (jobId: string) => command<unknown>('job.cancel', { job_id: jobId });
export const fetchSpendStatus = () => command<SpendStatus>('spend.status');
export const setSpendCap = (capUsd: number) => command<unknown>('spend.set_cap', { cap_usd: capUsd });
/** A number sets the UTC-month cap; `null` clears it. The daily cap is left as it is. */
export const setMonthlySpendCap = (capUsd: number | null) => command<unknown>('spend.set_cap', { monthly_cap_usd: capUsd });
/**
 * Records the user's invoice beside the engine's amounts. It writes the invoice record and matched rows' invoice
 * amounts only: it contacts no provider, spends nothing, and changes no cap, reservation or counted amount.
 * Rejects `invoice_ref_conflict` when the reference was recorded with different details, `invalid_input` on bad fields.
 */
export const reconcileSpend = (input: SpendReconcileInput) => command<SpendReconcileResult>('spend.reconcile', input);

/** One row of the device-allowance audit (`spend.allowance` lists the last 20, newest first). */
export interface DeviceAllowanceAuditRow {
  id: number;
  at: string;
  who: string;
  device_id: string;
  old_usd: number | null;
  new_usd: number | null;
  note: string | null;
}
/**
 * `spend.allowance` / `spend.set_allowance`: THIS device's offline allowance only. `shared_allowance` says
 * no allowance is shared between computers (`available: false`, with the engine's reason).
 */
export interface DeviceAllowanceView {
  device_id: string;
  device_allowance_usd: number | null;
  /** Reservations made since the allowance was last set; `null` when no allowance is set. */
  counted_usd: number | null;
  remaining_usd: number | null;
  set_at: string | null;
  shared_allowance: { available: boolean; reason: string };
  audit: DeviceAllowanceAuditRow[];
}
export const fetchSpendAllowance = () => command<DeviceAllowanceView>('spend.allowance');
/** `null` clears this device's allowance. Setting or clearing restarts the counted baseline. */
export const setSpendAllowance = (amountUsd: number | null, note?: string) =>
  command<DeviceAllowanceView>('spend.set_allowance', { device_allowance_usd: amountUsd, ...(note ? { note } : {}) });

/**
 * Permanently removes one output from the library and its paired gallery file from disk. Not undoable.
 * Give `asset_id` or `path`. `removed` is false when nothing matched.
 */
export const deleteAsset = (target: { asset_id?: string; path?: string }) =>
  command<{ removed: boolean }>('asset.delete', { ...target });

/* ---------- Library search and asset import (engine `library.search` and `asset.import`) ---------- */

function libraryCommand<T>(name: string, input: Record<string, unknown>): Promise<T> {
  return command<T>(name, input);
}

/**
 * Searches the library index with the engine's query syntax (free text, phrases, `kind:`, `origin:`,
 * `rights:`, `venture:` and the rest). An invalid query rejects with `invalid_input`; read it with errorText.
 */
export function searchLibrary(request: LibrarySearchRequest): Promise<LibrarySearchResult> {
  const input: Record<string, unknown> = { query: request.query };
  if (request.limit !== undefined) input.limit = request.limit;
  if (request.cursor) input.cursor = request.cursor;
  if (request.filters) input.filters = request.filters;
  return libraryCommand<LibrarySearchResult>('library.search', input);
}

/** Dry run of an import: validates it the way the engine does and reports what would be recorded. Writes nothing. */
export function previewAssetImport(request: AssetImportRequest): Promise<AssetImportDryRun> {
  return libraryCommand<AssetImportDryRun>('asset.import', { ...request, dry_run: true });
}

/** Copies the file into the library with its facts and scope. The source file is left in place. */
export function importAsset(request: AssetImportRequest): Promise<AssetImportResult> {
  return libraryCommand<AssetImportResult>('asset.import', { ...request, dry_run: false });
}

/** URL a webview can load for an on-disk asset (inline data, blob and http URLs pass through). */
export function assetUrl(path: string): string {
  if (path.startsWith('data:') || path.startsWith('blob:') || /^https?:/.test(path)) return path;
  return mediaGenClient().assetUrl(path);
}

// Local image: the engine's `local_image.*` commands, reached through the same `command` path as the rest of the engine.

export async function fetchLocalImageProfiles(): Promise<{ profiles: LocalImageProfile[] }> {
  return command<{ profiles: LocalImageProfile[] }>('local_image.profiles');
}

export async function fetchLocalImageRuntime(): Promise<LocalImageRuntimeStatus> {
  return command<LocalImageRuntimeStatus>('local_image.runtime');
}

export async function setLocalImageIdleUnload(seconds: number): Promise<LocalImageRuntimeStatus> {
  return command<LocalImageRuntimeStatus>('local_image.set_idle_unload', { seconds });
}

export async function setLocalImageMemoryMode(mode: string): Promise<LocalImageRuntimeStatus> {
  return command<LocalImageRuntimeStatus>('local_image.set_memory_mode', { mode });
}

export async function submitLocalImage(request: LocalImageSubmitRequest): Promise<{ job: LocalImageJob }> {
  // The engine's SubmitRequest object is the whole input.
  return command<{ job: LocalImageJob }>('local_image.submit', { ...request });
}

export async function getLocalImageJob(jobId: string): Promise<LocalImageJobSnapshot> {
  return command<LocalImageJobSnapshot>('local_image.get', { job_id: jobId });
}

export async function listLocalImages(opts: { conversationId?: string; limit?: number } = {}): Promise<LocalImageListResponse> {
  return command<LocalImageListResponse>('local_image.list', { conversation_id: opts.conversationId ?? null, limit: opts.limit ?? null });
}

export async function cancelLocalImage(jobId: string): Promise<{ job: LocalImageJob }> {
  return command<{ job: LocalImageJob }>('local_image.cancel', { job_id: jobId });
}

export async function resumeLocalImage(jobId: string): Promise<{ job: LocalImageJob }> {
  return command<{ job: LocalImageJob }>('local_image.resume', { job_id: jobId });
}

export async function importLocalImageReference(path: string): Promise<{ reference: LocalImageReference }> {
  return command<{ reference: LocalImageReference }>('local_image.import_reference', { path });
}

export async function installLocalImageProfile(profileId: string): Promise<{ started: boolean }> {
  return command<{ started: boolean }>('local_image.install', { profile_id: profileId });
}

/** Pause a running profile install; partial downloads are kept and installing again resumes them. */
export async function pauseLocalImageInstall(profileId: string): Promise<{ paused: boolean }> {
  return command<{ paused: boolean }>('local_image.install.pause', { profile_id: profileId });
}

/** Cancel a running profile install and discard its partial downloads. */
export async function cancelLocalImageInstall(profileId: string): Promise<{ cancelled: boolean }> {
  return command<{ cancelled: boolean }>('local_image.install.cancel', { profile_id: profileId });
}

// ---------------------------------------------------------------- request routing (media.route, media.route_prefs)

export type RoutePrivacy = 'any' | 'local_only';
export type RouteSpend = 'ask' | 'free_only' | 'cheapest';

export interface RoutePreferences {
  privacy: RoutePrivacy;
  spend: RouteSpend;
  /** Stored, echoed, not applied: the catalogue holds no quality facts. */
  quality: 'best' | 'balanced' | 'economy';
  /** Stored, echoed, not applied: the catalogue holds no latency facts. */
  time: 'fastest' | 'balanced' | 'relaxed';
  ordering_note?: string;
}

export interface RouteAlternative {
  provider: string;
  model: string;
  cloud: boolean;
  states: { configured: boolean; installed: boolean | null; qualified: boolean | null; usable: boolean };
  est_usd: number;
  est_source: string;
  eligible: boolean;
  reason: string;
}

export type RouteOutcome = 'chosen' | 'route_choice_required' | 'no_usable_route' | 'ambiguous_route';

export interface RouteResult {
  kind: string;
  outcome: RouteOutcome;
  route_choice_required: boolean;
  basis: string;
  chosen: RouteAlternative | null;
  alternatives: RouteAlternative[];
  preferences: RoutePreferences;
  selection: { route: string; model: string | null } | null;
  selection_note: string | null;
  note: string;
  preferences_not_applied?: string[];
  preferences_not_applied_reason?: string;
}

export interface RoutePrefsResult {
  preferences: RoutePreferences;
  saved_under: string;
}

/** `media.route` input. Provider and model are sent only when the user picked a route. Never submits. */
export type RouteInput = { kind: MediaKind; provider?: string; model?: string; privacy?: 'local_only' };

function routingCommand<T>(method: 'media.route' | 'media.route_prefs', input: Record<string, unknown>): Promise<T> {
  return command<T>(method, input);
}

/** Which route a request kind would take, with every alternative's four states. Contacts no provider and spends nothing. */
export function routeMedia(input: RouteInput): Promise<RouteResult> {
  return routingCommand<RouteResult>('media.route', { ...input });
}

export function fetchRoutePrefs(): Promise<RoutePrefsResult> {
  return routingCommand<RoutePrefsResult>('media.route_prefs', {});
}

/** Save the given routing preferences over the saved ones; returns the saved set. */
export function saveRoutePrefs(patch: Partial<Pick<RoutePreferences, 'privacy' | 'spend'>>): Promise<RoutePrefsResult> {
  return routingCommand<RoutePrefsResult>('media.route_prefs', { ...patch });
}

// ---------------------------------------------------------------------------
// Recipe templates: studio.recipes.templates, template_compose, template_preflight,
// template_approve (through approveDraft only), character_batch, batch_review, and
// studio.approvals.challenge for a draft's request hash. Shapes follow docs/ENGINE-CLI.md.
// Outside Tauri the calls go to the browser-preview mock (mockRecipeTemplates) directly.
// ---------------------------------------------------------------------------

export type RecipeSlotType = 'image' | 'video';

export interface RecipeTemplateSlot {
  position: number;
  name: string;
  type: RecipeSlotType;
  engine_role: string;
  required: boolean;
  max: number;
  role: string;
}

export interface RecipeTemplateRoute {
  provider: string;
  model: string;
  served: boolean;
}

export interface RecipeModerationNote {
  text: string;
  generic: boolean;
  source: string | null;
}

export interface RecipeModerationGuide {
  notes: RecipeModerationNote[];
  generic: RecipeModerationNote[];
  note: string;
}

export interface RecipeStagedResolution {
  param: string;
  draft: string;
  final: string;
  identical_prompt: boolean;
  second_requires_first_approval: boolean;
}

export interface RecipeTemplate {
  template: string;
  title: string;
  kind: string;
  served: boolean;
  routes: RecipeTemplateRoute[];
  slots: RecipeTemplateSlot[];
  prompt: { lead: string; lock: string };
  staged_resolution: RecipeStagedResolution | null;
  moderation: RecipeModerationGuide;
}

export interface RecipeBatchSpec {
  title: string;
  kind: string;
  max_items: number;
}

export interface RecipeTemplatesResult {
  ok: boolean;
  templates: RecipeTemplate[];
  batches: Record<string, RecipeBatchSpec>;
  slot_rules: string;
}

export interface RecipeTemplateSlotInput {
  type: RecipeSlotType;
  value: string;
  slot?: number;
}

export interface RecipeTemplateInput {
  template: string;
  slots: RecipeTemplateSlotInput[];
  provider?: string;
  model?: string;
  params?: Record<string, unknown>;
  prompt?: string;
  staged?: boolean;
}

export interface RecipeRequest {
  kind: string;
  provider: string;
  model: string;
  prompt: string;
  params: Record<string, unknown>;
}

export interface RecipeRouteRef {
  provider: string;
  model: string;
  label?: string;
  endpoint?: string;
}

export interface RecipeComposedSlot {
  position: number;
  name: string;
  param: string;
  value: string;
}

export type RecipeStagePlan =
  | {
      available: true;
      param: string;
      options: string[];
      lowest: string;
      requested: string;
      single_stage: boolean;
      identical_prompt: boolean;
      second_requires_first_approval: boolean;
    }
  | { available: false; param: string; reason: string };

export interface RecipeViolation {
  position: number;
  slot: string;
  code: string;
  problem: string;
}

export interface RecipeComposeOk {
  ok: true;
  template: string;
  kind: string;
  route: RecipeRouteRef;
  slots: RecipeComposedSlot[];
  request: RecipeRequest;
  est_usd: number | null;
  would_submit?: boolean | null;
  moderation: RecipeModerationGuide;
  staged: RecipeStagePlan;
  warnings: string[];
}

/** A refusal: a result body with `ok: false`, not a thrown command error. */
export interface RecipeRefusal {
  ok: false;
  code: string;
  message: string;
  status?: string;
  template?: string;
  kind?: string;
  missing_routes?: Array<{ provider: string; model: string }>;
  violations: RecipeViolation[];
  route?: { provider: string; model: string };
}

export type RecipeComposeResult = RecipeComposeOk | RecipeRefusal;

export type RecipeDraftStage = 'single' | 'draft' | 'final';

export interface RecipeDraft {
  draft_id: string;
  stage: RecipeDraftStage;
  status?: string;
  requires_approval_of: string | null;
  est_usd: number | null;
  request_hash: string;
  request: RecipeRequest;
  approval_challenge?: { nonce: string; legacy_hash: string; input_hash: string };
}

export interface RecipePreflightOk {
  ok: true;
  template: string;
  kind: string;
  route: RecipeRouteRef;
  staged: boolean;
  stage_plan: RecipeStagePlan;
  note: string | null;
  drafts: RecipeDraft[];
  slots: RecipeComposedSlot[];
  moderation: RecipeModerationGuide;
  warnings: string[];
}

export type RecipePreflightResult = RecipePreflightOk | RecipeRefusal;

export interface RecipeApproveResult {
  ok: true;
  draft: { draft_id: string; status: string; stage: RecipeDraftStage; request_hash: string };
  generate: RecipeRequest;
}

export type RecipeBatchState = 'pending' | 'approved' | 'rejected' | 'refused';

export interface RecipeBatchRefusal {
  code: string;
  message: string;
}

export interface RecipeBatchItem {
  index: number;
  scenario: string;
  state: RecipeBatchState;
  draft_id: string | null;
  prompt?: string;
  est_usd?: number | null;
  identity_attached?: boolean | null;
  asset?: string | null;
  refusal: RecipeBatchRefusal | null;
  /** Only on the creation response: the item draft's single-use challenge (its legacy_hash is the request hash). */
  approval_challenge?: { nonce: string; legacy_hash: string; input_hash: string };
}

export interface RecipeBatchView {
  ok: true;
  batch_id: string;
  template: string;
  character_id: string;
  lock: unknown;
  review: { mode: string; compositing: string };
  counts: { items: number; pending: number; approved: number; rejected: number; refused: number };
  items: RecipeBatchItem[];
}

export interface RecipeBatchInput {
  character_id: string;
  provider: string;
  model: string;
  scenarios: string[];
  references?: string[];
}

export interface RecipeBatchDecision {
  item: number;
  decision: 'approve' | 'reject';
  approval_token?: string;
  approval_nonce?: string;
  asset?: string;
}

function recipeCall<T>(name: string, input: Record<string, unknown> = {}): Promise<T> {
  const full = name.startsWith('studio.') ? name : `studio.${name}`;
  return studio<T>(full, input);
}

export function recipeTemplatesList(): Promise<RecipeTemplatesResult> {
  return recipeCall<RecipeTemplatesResult>('recipes.templates');
}

export function recipeTemplateCompose(input: RecipeTemplateInput): Promise<RecipeComposeResult> {
  return recipeCall<RecipeComposeResult>('recipes.template_compose', { ...input });
}

export function recipeTemplatePreflight(input: RecipeTemplateInput): Promise<RecipePreflightResult> {
  return recipeCall<RecipePreflightResult>('recipes.template_preflight', { ...input });
}

/**
 * Approve one recipe-template draft as the user: approveDraft('recipe_templates', ...), which the host
 * turns into the engine's token-bound approval.
 */
export function approveRecipeTemplateDraft(draftId: string, displayedHash: string): Promise<RecipeApproveResult> {
  return approveDraft<RecipeApproveResult>('recipe_templates', draftId, displayedHash);
}

/** The request hash of a draft as the engine holds it, from a fresh single-use challenge (approves nothing by itself). */
export async function recipeDraftHash(draftId: string): Promise<string> {
  const challenge = await recipeCall<{ legacy_hash?: string }>('approvals.challenge', { kind: 'recipe_templates', draft_id: draftId });
  return challenge.legacy_hash ?? '';
}

export function recipeCharacterBatch(input: RecipeBatchInput): Promise<RecipeBatchView> {
  return recipeCall<RecipeBatchView>('recipes.character_batch', { ...input });
}

/** Read a batch back from the engine (no decisions), or apply reject decisions and return the new review. */
export function recipeBatchReview(batchId: string, decisions?: RecipeBatchDecision[]): Promise<RecipeBatchView> {
  return recipeCall<RecipeBatchView>('recipes.batch_review', decisions ? { batch_id: batchId, decisions } : { batch_id: batchId });
}

// ---------------------------------------------------------------------------
// Recipe graph catalogue: studio.recipes.catalogue (seeded graphs with validity, per-node served
// status and missing routes, and the plan's named list) and studio.recipes.catalogue_validate
// ({id} for a seeded graph, {graph} for an inline one). Read-only: nothing here runs a graph or
// spends. Outside Tauri the calls go to mockRecipeCatalogue (browser-mock-api.ts).
// ---------------------------------------------------------------------------

export type RecipeCatalogueServed = 'served' | 'unserved' | 'ask' | 'native' | 'invalid_route';

export interface RecipeCatalogueRoute {
  provider: string;
  model: string;
}

export interface RecipeCatalogueError {
  code: string;
  node: string | null;
  message: string;
}

export interface RecipeCatalogueNodeStatus {
  id: string;
  kind: string;
  output: string | null;
  status: RecipeCatalogueServed;
  /** {provider, model}; the string "ask" for a node that asks at run time; null for a native step. */
  route: RecipeCatalogueRoute | 'ask' | null;
  /** Present for served nodes only: whether a declared catalogue entry checked the params. */
  params_checked?: boolean;
  /** For unserved nodes: the preferred route first, then the alternatives. Empty otherwise. */
  missing_routes: RecipeCatalogueRoute[];
  /** The node ids this node reads (engine `rightkit-prompt-recipes` 0.2.0 and later). */
  depends_on?: string[];
}

export interface RecipeCatalogueGraph {
  id: string;
  title: string;
  source: Record<string, string>;
  inputs: Array<{ name: string; type: string }>;
  outputs: Array<{ name: string; from: string }>;
  review: { required: boolean; policy: string };
  valid: boolean;
  errors: RecipeCatalogueError[];
  nodes: RecipeCatalogueNodeStatus[];
  missing_routes: Array<RecipeCatalogueRoute & { node: string }>;
}

export interface RecipePlannedName {
  name: string;
  seeded: string | null;
  reason: string;
}

export interface RecipeCatalogueResult {
  ok: boolean;
  graphs: RecipeCatalogueGraph[];
  plan: string;
  plan_named: RecipePlannedName[];
  pipeline_templates_not_named_by_plan: Array<{ id: string; seeded: boolean }>;
}

export interface RecipeCatalogueValidation {
  ok: boolean;
  id: string;
  errors: RecipeCatalogueError[];
  nodes: RecipeCatalogueNodeStatus[];
}

function recipeCatalogueCall<T>(name: string, input: Record<string, unknown> = {}): Promise<T> {
  const full = name.startsWith('studio.') ? name : `studio.${name}`;
  return studio<T>(full, input);
}

/** Every seeded recipe graph, with validity, served status per node and the plan's named recipes. */
export function recipeCatalogueList(): Promise<RecipeCatalogueResult> {
  return recipeCatalogueCall<RecipeCatalogueResult>('recipes.catalogue');
}

/** Validate one seeded graph by id. Errors name the node they concern. Runs nothing. */
export function recipeCatalogueValidate(id: string): Promise<RecipeCatalogueValidation> {
  return recipeCatalogueCall<RecipeCatalogueValidation>('recipes.catalogue_validate', { id });
}

// RENTALS (begin): the rented-worker rentals panel. These calls never rent: a rental is created only by
// `studio.models.worker.action` `provision` (authorized, budgeted, spend-reserved), which the panel does not send.
// Engine: crates/rightkit-remote-compute/src/commands.rs (worker_action: teardown, resolve_rental) and rental.rs.

/** Every Vast rental with elapsed, accrued, the three limits (time left) and warnings. Read only. */
export const rentalsList = () => studio<RentalsListing>('models.rentals');

/** Destroy and settle every rental past a limit; reports what it did. Destroying needs VAST_API_KEY in the engine environment. */
export const rentalsEnforce = () => studio<RentalsEnforceReport>('models.rentals_enforce');

/** Destroy a rental's instance and settle it. `force` is required for a worker that is running (it interrupts the work). */
export const rentalTeardown = (workerId: string, force: boolean) =>
  studio<RentalWorkerRecord>('models.worker.action', {
    worker_id: workerId,
    action: 'teardown',
    ...(force ? { force: true } : {}),
  });

/** Resolve a create whose outcome is unknown: adopt the one instance labelled for the worker, or (confirmNone) release when none exists. */
export const rentalResolve = (workerId: string, confirmNone: boolean) =>
  studio<RentalWorkerRecord>('models.worker.action', {
    worker_id: workerId,
    action: 'resolve_rental',
    ...(confirmNone ? { confirm_none: true } : {}),
  });

/** Whether a Vast.ai key is held in the credential store. Never returns the key. */
export const vastKeyStatus = () => studio<{ vast_key_stored: boolean }>('models.worker.action', { action: 'vast_key_status' });

/** Save the Vast.ai key in the credential store (read back before success); an empty string removes it. Never echoes the key. */
export const setVastKey = (apiKey: string) =>
  studio<{ ok: boolean; vast_key_stored: boolean }>('models.worker.action', { action: 'set_vast_key', api_key: apiKey });
// RENTALS (end)
