// Shared runtime types extracted from the per-studio run-type modules.
// The five studio run manifests share the process shape. Image and video runs
// additionally share the eyes-gate shape.
import type { EyesStatus } from './gallery.js';

export interface ProcessInfo {
  command: string;
  args: string[];
  command_label: string;
  exit_code: number;
  signal: string | null;
}

// The eyes-gate block attached to visual run manifests.
export interface EyesGateBlock {
  required: boolean;
  verdict: EyesStatus;
  artifact_path: string;
  sidecar_path: string;
  hash: string | null;
  decided_at: string | null;
  cached: boolean;
  error: string | null;
}
