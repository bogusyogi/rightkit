import type { ProcessInfo } from './runtime.js';

export type HeyGenAvatarType = 'digital_twin' | 'photo' | 'prompt';

export interface HeyGenAvatarRequest {
  model_slug: 'heygen-digital-twin-create' | 'heygen-photo-avatar-create' | 'heygen-prompt-avatar-create';
  avatar_type: HeyGenAvatarType;
  project_slug: string;
  name: string;
  personal_use_only?: boolean;
  training_video_path?: string;
  training_video_url?: string;
  training_video_asset_id?: string;
  source_image_path?: string;
  source_image_url?: string;
  source_image_asset_id?: string;
  prompt?: string;
  reference_image_paths?: string[];
  reference_image_urls?: string[];
  reference_image_asset_ids?: string[];
  avatar_group_id?: string;
  avatar_id?: string;
  create_consent?: boolean;
  consent_redirect_url?: string;
  consent_text?: string;
}

export interface HeyGenAvatarDraft {
  schema_version: '1.0.0';
  draft_id: string;
  status: 'draft';
  execution_mode: 'preflight_only';
  created_at: string;
  request: HeyGenAvatarRequest;
  route: {
    slug: string;
    provider: 'heygen';
    label: string;
    avatar_type: HeyGenAvatarType;
    use: string;
    estimated_cost: string;
  };
  pipeline: {
    type: string;
    execution_note: string;
  };
  manifest_path: string;
}

export interface HeyGenAvatarRunRequest {
  manifest_path: string;
  operator_approved: boolean;
}

export interface HeyGenAvatarRun {
  schema_version: '1.0.0';
  run_id: string;
  draft_id: string;
  status: 'completed' | 'failed';
  execution_mode: 'operator_approved_heygen_avatar_run';
  operator_approved_at: string;
  started_at: string;
  completed_at: string;
  draft_manifest_path: string;
  output_path: string;
  run_manifest_path: string;
  route: HeyGenAvatarDraft['route'];
  request: HeyGenAvatarRequest;
  process: ProcessInfo;
  stdout: string;
  stderr: string;
}

export type HeyGenAvatarRunEvent =
  | {
      type: 'run_started';
      run_id: string;
      draft_id: string;
      output_path: string;
      run_manifest_path: string;
    }
  | {
      type: 'stdout' | 'stderr';
      run_id: string;
      text: string;
    }
  | {
      type: 'run_completed' | 'run_failed';
      run_id: string;
      run: HeyGenAvatarRun;
    };
