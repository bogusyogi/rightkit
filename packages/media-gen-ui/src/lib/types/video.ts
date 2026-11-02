import type { Brand } from './gallery.js';
import type { EyesGateBlock, ProcessInfo } from './runtime.js';

export type VideoRefKind = 'flf' | 'ingredient' | 'lipsync' | 'motion_control' | 'avatar_video' | 'video_lipsync' | 'none';
export type VideoReferenceKind = 'image' | 'audio' | 'video';

export interface VideoModel {
  slug: string;
  backend: 'fal' | 'wavespeed' | 'heygen';
  label: string;
  use: string;
  cost_per_second_usd: number;
  default_duration: number;
  accepts_refs: boolean;
  ref_kind: VideoRefKind;
  min_refs: number;
  max_refs: number;
  frame_roles: string[];
  default_resolution?: string;
  cost_per_second_by_resolution?: Record<string, number>;
}

export interface VideoContractTemplate {
  slug: string;
  name: string;
  path: string;
  summary: string;
}

export interface VideoReference {
  path: string;
  basename: string;
  source: 'content_refs' | 'codex_generated' | 'tts_generated';
  size_bytes: number;
  kind: VideoReferenceKind;
  duration_seconds?: number | null;
}

export interface VideoReferencesResponse {
  references: VideoReference[];
  reference_cap: number;
  roots: string[];
  diagnostics: Array<{
    severity: string;
    path?: string;
    message: string;
  }>;
}

export type VideoStudioRefs =
  | {
      ref_kind: 'flf';
      frame_roles: string[];
      flf: {
        start_frame_path: string;
        end_frame_path?: string;
      };
    }
  | {
      ref_kind: 'ingredient';
      frame_roles: string[];
      ingredient: {
        ingredient_refs: string[];
      };
    }
  | {
      ref_kind: 'motion_control';
      frame_roles: string[];
      motion_control: {
        start_frame_path: string;
      };
    }
  | {
      ref_kind: 'lipsync';
      frame_roles: string[];
      lipsync: {
        face_image_path: string;
        audio_track_path: string;
      };
    }
  | {
      ref_kind: 'avatar_video';
      frame_roles: string[];
      avatar_video: {
        avatar_id: string;
        script?: string;
        voice_id?: string;
        audio_track_path?: string;
      };
    }
  | {
      ref_kind: 'video_lipsync';
      frame_roles: string[];
      video_lipsync: {
        source_video_path: string;
        audio_track_path: string;
      };
    }
  | {
      ref_kind: 'none';
      frame_roles: string[];
      none: Record<string, never>;
    };

export interface VideoStudioPreflightRequest {
  brand: Brand;
  project_slug: string;
  model_slug: string;
  prompt: string;
  camera_move?: string;
  marketing_template?: string;
  aesthetic?: string;
  subject?: string;
  setting?: string;
  action?: string;
  mood?: string;
  shot_type?: string;
  product_spec?: string;
  colors?: string[];
  psych_effect?: string;
  lighting?: string;
  negatives?: string[];
  subject_motion?: string;
  scene_motion?: string;
  transition?: string;
  continuity_locks?: string[];
  text_overlay?: string;
  audio_recipe?: string;
  qa_rubric?: string;
  composer_meta?: Record<string, unknown>;
  workflow?: Record<string, unknown>;
  generation_params?: Record<string, unknown>;
  shot_contract_template?: string;
  shot_contract_inline?: Record<string, unknown>;
  shot_contract_overrides?: Record<string, unknown>;
  aspect: string;
  duration: number;
  resolution?: string;
  start_frame_path?: string;
  end_frame_path?: string;
  ingredient_refs?: string[];
  motion_control_ref?: string;
  face_image_path?: string;
  audio_track_path?: string;
  source_video_path?: string;
  avatar_id?: string;
  script?: string;
  voice_id?: string;
}

export interface VideoStudioDraft {
  schema_version: '1.0.0';
  draft_id: string;
  status: 'draft';
  execution_mode: 'preflight_only';
  created_at: string;
  request: VideoStudioPreflightRequest & {
    venture: string;
    refs: VideoStudioRefs;
  };
  route: VideoModel;
  pipeline: {
    type: string;
    execution_note: string;
  };
  resolved_shot_contract: Record<string, unknown>;
  composed_prompt: string;
  estimated_cost_usd: number;
  manifest_path: string;
}

export interface VideoStudioRunRequest {
  manifest_path: string;
  operator_approved: boolean;
}

export interface VideoStudioRun {
  schema_version: '1.0.0';
  run_id: string;
  draft_id: string;
  status: 'completed' | 'failed' | 'needs_eyes';
  execution_mode: 'operator_approved_router_run';
  operator_approved_at: string;
  started_at: string;
  completed_at: string;
  draft_manifest_path: string;
  output_path: string;
  run_manifest_path: string;
  route: VideoModel;
  request: VideoStudioDraft['request'];
  eyes_gate: EyesGateBlock | null;
  process: ProcessInfo;
  stdout: string;
  stderr: string;
}

export type VideoStudioRunEvent =
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
      type: 'run_completed' | 'run_failed' | 'run_needs_eyes';
      run_id: string;
      run: VideoStudioRun;
    };
