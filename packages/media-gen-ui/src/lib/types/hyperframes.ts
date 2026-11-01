import type { ProcessInfo } from './runtime';

export interface HyperFramesStudioRequest {
  project_slug: string;
  project_dir: string;
  entry_file?: string;
  fps?: number;
  quality?: 'draft' | 'standard' | 'high';
  workers?: number;
}

export interface HyperFramesStudioDraft {
  schema_version: '1.0.0';
  draft_id: string;
  status: 'draft';
  execution_mode: 'preflight_only';
  created_at: string;
  request: HyperFramesStudioRequest & {
    entry_file: string;
    entry_path: string;
    fps: number;
    quality: 'draft' | 'standard' | 'high';
    workers: number;
  };
  route: {
    slug: 'hyperframes-cli';
    provider: 'hyperframes';
    label: string;
    use: string;
  };
  pipeline: {
    type: string;
    execution_note: string;
  };
  manifest_path: string;
}

export interface HyperFramesStudioRunRequest {
  manifest_path: string;
  operator_approved: boolean;
}

export interface HyperFramesStudioRun {
  schema_version: '1.0.0';
  run_id: string;
  draft_id: string;
  status: 'completed' | 'failed';
  execution_mode: 'operator_approved_hyperframes_run';
  operator_approved_at: string;
  started_at: string;
  completed_at: string;
  draft_manifest_path: string;
  output_path: string;
  run_manifest_path: string;
  route: HyperFramesStudioDraft['route'];
  request: HyperFramesStudioDraft['request'];
  process: ProcessInfo;
  stdout: string;
  stderr: string;
}

export type HyperFramesStudioRunEvent =
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
      run: HyperFramesStudioRun;
    };
