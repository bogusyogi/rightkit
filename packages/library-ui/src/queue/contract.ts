// TS mirror of src-tauri/src/contract.rs — keep in sync.

export type Kind =
  | "reel"
  | "instagram_post"
  | "carousel"
  | "youtube"
  | "tiktok"
  | "threads"
  | "facebook"
  | "video_url"
  | "media_url"
  | "article"
  | "local_file"
  | "unknown";

export type Stage =
  | "queued"
  | "detecting"
  | "downloading"
  | "fetched"
  | "transcribing"
  | "writing"
  | "done"
  | "error"
  | "cancelled";

export type ErrorKind =
  | "needs_auth"
  | "blocked"
  | "rate_limited"
  | "no_media"
  | "no_text_layer"
  | "unsupported"
  | "timeout"
  | "cancelled"
  | "other";

type MediaAction = "transcribe" | "download" | "both" | "images";
export type VideoQuality = "p720" | "p1080" | "p2160" | "source";
export type AudioQuality = "best" | "compact";

/** Result of the `submit_profile_images` command (#248 profile batch). */
export interface ProfileImagesResponse {
  profile: string;
  limit: number;
  found: number;
  accepted: number;
  job_ids: string[];
  rejected: string[];
}

export interface MediaSpec {
  action: MediaAction;
  video?: VideoQuality | null;
  audio?: AudioQuality | null;
}

export interface JobInfo {
  id: string;
  input: string;
  kind: Kind;
  stage: Stage;
  pct: number;
  title?: string | null;
  error?: string | null;
  error_kind?: ErrorKind | null;
  needs_auth?: string | null;
  media?: MediaSpec | null;
  /** WS-6: queue attempts that transcribe a saved download carry the target
   *  library row's id here. The attempt keeps its own queue id; completion
   *  writes into the existing history row instead of creating a new one. */
  attach_to?: string | null;
}

/** Frontend-only view of a job. Extends the wire JobInfo with `msg`, which is
 *  populated by merging JobEvent updates in-memory and is never serialised by Rust. */
export interface DisplayJob extends JobInfo {
  msg?: string | null;
  /** Set when a cancel request is in flight; cleared when the engine's
   *  terminal acknowledgement (or the request's failure) arrives. */
  cancelling?: boolean;
}

export interface JobEvent {
  id: string;
  stage: Stage;
  pct: number;
  msg?: string | null;
  error?: string | null;
  error_kind?: ErrorKind | null;
  needs_auth?: string | null;
}

export interface JobKindEvent {
  id: string;
  kind: Kind;
}

/** Shell-visible engine lifecycle state, emitted on `engine://state`. See
 *  docs/plans/2026-06-28-sidecar-crash-recovery.md. `recovering` covers the
 *  window between an unexpected sidecar exit and the replacement reaching
 *  `ready`; `down` is terminal until the app is relaunched. */
type EngineLifecycleState = "booting" | "ready" | "recovering" | "down";

export interface EngineLifecycleEvent {
  state: EngineLifecycleState;
  generation: number;
  attempt: number;
  restart_count_window: number;
  last_exit_code?: number | null;
  last_exit_reason?: string | null;
  recovery_ms?: number | null;
  message: string;
}

export interface HistoryRow {
  id: string;
  url: string;
  kind: Kind;
  title?: string | null;
  lang?: string | null;
  duration_s?: number | null;
  created_at: string;
  txt_path?: string | null;
  md_path?: string | null;
  srt_path?: string | null;
  slide_dir?: string | null;
  snippet?: string | null;
  media_path?: string | null;
  media_meta?: string | null;
  media_action?: string | null;
}

export type TranscriptVariantKind =
  | "text"
  | "md_text"
  | "timed_text"
  | "timed_md_text"
  | "srt";

export interface TranscriptVariantInfo {
  kind: TranscriptVariantKind;
  bytes: number;
}

export interface TranscriptVariant {
  id: string;
  kind: TranscriptVariantKind;
  text: string | null;
}

export interface TranscriptDetail {
  id: string;
  url: string;
  kind: Kind;
  title?: string | null;
  text: string;
  md_text?: string | null;
  timed_text?: string | null;
  timed_md_text?: string | null;
  srt?: string | null;
  slides: string[];
  media_path?: string | null;
  media_meta?: string | null;
  media_action?: string | null;
  /** Variants the row has, with sizes. `text` is only a preview until fetched. */
  variants?: TranscriptVariantInfo[];
  /** Seconds where Free-tier transcription stopped; absent = full source. */
  truncated_at_s?: number | null;
}

export interface Settings {
  output_dir: string;
  downloads_dir?: string | null;
  cookies_path?: string | null;
  ort_dylib_path?: string | null;
  keep_media: boolean;
  /** Save a trimmed `.fit.md` (focused article copy) for each article. */
  article_fit_copy: boolean;
  /** Minutes the speech model stays loaded after the last job; 0 never unloads. */
  model_idle_minutes?: number;
  /** Optional override for the locked Parakeet TDT model directory. Unset
   * uses the seeded app-data model. */
  model_dir?: string | null;
}

export interface DoctorReport {
  app_version?: string;
  cli_contract_version?: number;
  capability_version?: number;
  capabilities?: string[];
  directml: boolean;
  model_ok: boolean;
  ort_dylib_ok: boolean;
  ytdlp: boolean;
  ffmpeg: boolean;
  cookies_present: boolean;
  notes: string[];

  // Capability layer (additive for agent reach / headless CLI)
  asr_engine?: string;
  active_asr_backend?: string | null;
  asr_timings_supported?: boolean;
  tdt_model_ok?: boolean;
  article_backends?: string[];
  ocr_available?: boolean;
  supported_modalities?: string[];
  routing_note?: string | null;
}

export interface WhisperRuntimeStatus {
  os: string;
  arch: string;
  gpu_names: string[];
  preferred_backends: string[];
  installed_backend?: string | null;
  cli_path?: string | null;
  runtime_root: string;
  model_ok: boolean;
  model_path: string;
  tdt_model_ok: boolean;
  tdt_model_path?: string | null;
  /** TDT can emit per-token timings (SRT/VTT): true on Windows and macOS. */
  tdt_has_timing: boolean;
}

export interface AuthStatus {
  platform: string;
  profile_dir: string;
  profile_present: boolean;
  exported_cookies_path?: string | null;
  exported_cookies_present: boolean;
  last_error?: string | null;
}

/** iCloud phone → Mac queue sync status (Settings → General). */
export interface CloudSyncStatus {
  enabled: boolean;
  /** "off" | "syncing" | "ready" | "unavailable" | "consent_needed" (the
   * signed-in iCloud account changed; nothing syncs until the user answers
   * with `cloud_sync_grant_consent({ merge })`). */
  state: string;
  /** Why it's unavailable (no CloudKit transport, account offline, ...). */
  detail?: string | null;
  /** This build can reach CloudKit (macOS only). */
  supported: boolean;
}

/** WS-9: `pro_bridge_status` — the two independent Apple-continuity
 * channels (Keychain credential sync + backend Pro resolution). macOS-only
 * command; callers must gate on `platform() === "macos"` (or
 * `CloudSyncStatus.supported`). */
export interface ProBridgeStatus {
  /** The bundled credentials helper answered its status probe. */
  keychainReady: boolean;
  keychainDetail?: string | null;
  /** True while this device must not consume the shared bridge. */
  consumptionSuppressed: boolean;
  keychainSyncEnabled: boolean;
  proResolutionEnabled: boolean;
  /** "off" | "blocked_no_keychain" | "ok" */
  proResolutionStatus: string;
}
