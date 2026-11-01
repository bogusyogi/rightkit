import type { AssetFacts, AssetScope } from './library';

export type ArtifactType = 'image' | 'video' | 'audio' | 'unknown';
export type Brand = 'DD' | 'RH' | 'TS' | 'HR' | 'DV' | 'KDP' | 'NA';
export type Visibility = 'primary' | 'intermediate';
export type MetadataStatus = 'complete' | 'incomplete' | 'malformed' | 'missing_media';
export type EyesStatus = 'unknown' | 'pending' | 'approved' | 'rejected' | 'rework' | 'stale';
export type AutoJuryStatus = 'unknown' | 'pending' | 'passed' | 'blocked' | 'warn_only';
export type OverallStatus = 'draft' | 'needs_review' | 'approved' | 'rejected' | 'exported';

export interface GalleryArtifact {
  schema_version: '1.0.0';
  artifact_id: string;
  artifact_type: ArtifactType;
  absolute_path: string;
  project_slug: string;
  brand: Brand;
  visibility: Visibility;
  metadata_status: MetadataStatus;
  approval: {
    eyes: EyesStatus;
    auto_jury: AutoJuryStatus;
    overall: OverallStatus;
  };
  generation: {
    provider: string | null;
    model: string | null;
    chat_tool_version: string | null;
    prompt: string | null;
    aspect: string | null;
    cost_usd: number | null;
  };
  sidecars: {
    eyes_path: string | null;
    sidecar_path: string | null;
    verdict_path: string | null;
    manifest_path: string | null;
  };
  diagnostics: Array<string | Record<string, unknown>>;
  raw_sidecars?: Record<string, unknown>;
  /** Origin, evidence and rights, three separate facts. Absent on engines that predate them. */
  asset_facts?: AssetFacts | null;
  /** Venture, channel and account ids; null when none were recorded. */
  scope?: AssetScope | null;
  /** Slug of the engine caller (the host app, the host app, an agent session) that registered the file; null when none did. */
  caller?: string | null;
  /** How that caller named itself, for "From the host app". */
  caller_label?: string | null;
  indexed_at: string;
}

export interface GalleryReport {
  schema_version: '1.0.0';
  indexed_artifacts_total: number;
  metadata_incomplete_count: number;
  malformed_file_count: number;
  missing_media_count: number;
  skipped_path_count: number;
  fatal_error_count: number;
  elapsed_ms: number;
  scanned_roots: string[];
  /** Matching artifacts before paging; older engines omit it. */
  total_available?: number;
  offset?: number;
  has_more?: boolean;
}

export interface GalleryResponse {
  artifacts: GalleryArtifact[];
  report: GalleryReport;
}
