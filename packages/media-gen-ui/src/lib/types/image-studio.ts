import type { Brand } from './gallery.js';
import type { ImageIntent } from './image.js';
import type { EyesGateBlock, ProcessInfo } from './runtime.js';
import type { AestheticLora, StudioCharacter } from './workflow.js';

export interface ImageStudioLora {
  path: string;
  scale: number;
  base_model: string;
}

export interface ImageStudioRequest {
  mode: 'text_to_image' | 'image_to_image';
  brand: Brand;
  project_slug: string;
  intent: string;
  aspect: string;
  prompt: string;
  references: string[];
  character_id?: string;
  character?: StudioCharacter;
  aesthetic_lora_id?: string;
  aesthetic_lora?: AestheticLora;
  loras?: ImageStudioLora[];
  character_reference_diagnostics?: Array<{
    severity: string;
    message: string;
  }>;
  resolution?: string | null;
  quality?: string | null;
  negativePrompt?: string | null;
  style?: string | null;
  size?: string | null;
  image_size?: string | { width: number; height: number } | null;
  generation_params?: Record<string, unknown>;
  notes?: string | null;
  workflow?: Record<string, unknown>;
}

export interface ImageStudioDraft {
  schema_version: '1.0.0';
  draft_id: string;
  status: 'draft';
  execution_mode: 'preflight_only';
  created_at: string;
  request: ImageStudioRequest;
  route: ImageIntent & { intent: string };
  pipeline: {
    type: string;
    execution_note: string;
  };
  manifest_path: string;
}

export interface ImageStudioRunRequest {
  manifest_path: string;
  operator_approved: boolean;
}

export interface ImageStudioRun {
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
  route: ImageIntent & { intent: string };
  request: ImageStudioRequest;
  eyes_gate: EyesGateBlock | null;
  process: ProcessInfo;
  stdout: string;
  stderr: string;
}

export type ImageStudioRunEvent =
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
      run: ImageStudioRun;
    };
