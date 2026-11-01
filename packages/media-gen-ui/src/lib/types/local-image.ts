export type LocalImageJobState = 'queued' | 'loading' | 'submitting' | 'running' | 'finalizing' | 'succeeded' | 'failed' | 'cancelled' | 'cancelling' | 'interrupted' | 'paused';

export interface LocalImageOverrides {
  sample_steps?: number;
  sample_method?: string;
  scheduler?: string;
  txt_cfg?: number;
  negative_prompt?: string;
}

export interface LocalImageSubmitRequest {
  request_id: string;
  conversation_id?: string;
  parent_message_id?: string;
  profile_id: string;
  prompt: string;
  negative_prompt?: string;
  width: number;
  height: number;
  seed?: number | null;
  reference_ids: string[];
  overrides?: LocalImageOverrides;
  retry_of?: string;
}

export interface LocalImageJob {
  id: string;
  request_id: string;
  conversation_id: string;
  message_id: string | null;
  parent_message_id: string | null;
  profile_id: string;
  profile_revision: number;
  prompt: string;
  negative_prompt: string;
  width: number;
  height: number;
  seed: number;
  reference_ids: string[];
  overrides: LocalImageOverrides;
  state: LocalImageJobState;
  stage: string;
  progress: number | null;
  queue_position: number;
  provider_job_id: string | null;
  runtime_session_id: string | null;
  error_code: string | null;
  error_message: string | null;
  retry_of: string | null;
  created_at: string;
  updated_at: string;
  finished_at: string | null;
}

export interface LocalImageJobEvent {
  job_id: string;
  revision: number;
  state: LocalImageJobState;
  stage: string;
  progress: number | null;
  runtime_session_id: string | null;
  error_code: string | null;
  at: string;
}

export interface LocalImageArtifact {
  id: string;
  job_id: string;
  path: string;
  sidecar_path: string | null;
  manifest_path: string | null;
  sha256: string | null;
  bytes: number;
  width: number;
  height: number;
  created_at: string;
}

export interface LocalImageMessage {
  id: string;
  conversation_id: string;
  parent_id: string | null;
  role: 'user' | 'assistant' | 'system';
  kind: 'prompt' | 'result' | 'status';
  prompt: string | null;
  reference_ids: string[];
  job_id: string | null;
  created_at: string;
}

export interface LocalImageConversation {
  id: string;
  title: string | null;
  created_at: string;
}

export interface LocalImageConversationSnapshot {
  conversation: LocalImageConversation;
  messages: LocalImageMessage[];
  jobs: LocalImageJob[];
  artifacts: Record<string, LocalImageArtifact[]>;
}

export interface LocalImageReference {
  id: string;
  stored_path: string;
  sha256: string;
  bytes: number;
  width: number;
  height: number;
  mime: string;
  created_at: string;
}

export interface LocalImageComponent {
  kind: string;
  file: string;
  bytes: number | null;
  sha256: string | null;
  state: 'missing' | 'staged' | 'installed' | 'verifying' | 'failed';
  required: boolean;
}

export interface LocalImageProfile {
  id: string;
  revision: number;
  label: string;
  role: string;
  description: string;
  license: { name: string; url: string; commercial: boolean };
  tasks: string[];
  dimension_multiple: number;
  defaults: {
    sample_steps: number;
    sample_method: string;
    scheduler: string | null;
    txt_cfg: number;
    negative_prompt: string;
    width: number;
    height: number;
  };
  components: LocalImageComponent[];
  install_state: 'not_installed' | 'partial' | 'installed' | 'installing' | 'failed';
  qualification: Record<string, string>;
  total_bytes: number;
}

export interface LocalImageRuntimeStatus {
  state: 'stopped' | 'starting' | 'ready' | 'degraded' | 'missing_binary' | 'missing_model';
  pid: number | null;
  session_id: string | null;
  profile_id: string | null;
  port: number | null;
  capabilities: Record<string, unknown> | null;
  last_error: string | null;
  backend_binary: string;
  backend_present?: boolean;
  loaded?: boolean;
  memory_mb?: number | null;
  idle_unload_seconds?: number;
  memory?: LocalImageMemoryStatus;
}

export type LocalImageMemoryMode = 'auto' | 'performance' | 'balanced' | 'low';

/** Memory mode of the local engine. Every peak is an estimate until measured. */
export interface LocalImageMemoryStatus {
  /** What is configured (auto, performance, balanced, low). */
  requested?: string;
  source?: string;
  /** What it resolves to on this machine. */
  resolved?: 'performance' | 'balanced' | 'low';
  total_gb?: number | null;
  flags?: string[];
  estimated_peak_gb?: number;
  estimated_peaks_gb?: Record<string, number>;
  /** Mode of the running server, null when unloaded. */
  running?: string | null;
  error?: string;
}

export interface LocalImageJobSnapshot {
  job: LocalImageJob;
  events: LocalImageJobEvent[];
  artifacts: LocalImageArtifact[];
  message: LocalImageMessage | null;
}

export interface LocalImageListResponse {
  conversations: LocalImageConversationSnapshot[];
}

export interface LocalImageInstallProgress {
  profile_id: string;
  component: string;
  file: string;
  bytes_done: number;
  bytes_total: number;
  state: 'downloading' | 'verifying' | 'activating' | 'installed' | 'failed';
  error: string | null;
}

export interface LocalImageJobEventPayload {
  job_id: string;
  revision: number;
  state: LocalImageJobState;
  stage: string;
  progress: number | null;
  runtime_session_id: string | null;
  error_code: string | null;
}
