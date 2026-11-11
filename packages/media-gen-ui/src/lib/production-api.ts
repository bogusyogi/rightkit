import { studio } from './api.js';

export type LockStatus = 'ready' | 'missing' | 'optional';
export type HandoffStatus = 'ready' | 'ready_with_warnings' | 'blocked';

export interface ChannelSpec {
  id: string;
  label: string;
  short: string;
  aspect: string;
  duration_s: number;
  caption_style: string;
  cta: string;
}

export interface ProductionTemplates {
  brands: string[];
  default_brand: string;
  channels: ChannelSpec[];
  aspects: string[];
  audio_modes: string[];
  camera_profiles: string[];
  lenses: string[];
  movements: string[];
  defaults: {
    project_slug: string;
    product_name: string;
    offer: string;
    audience: string;
    promise: string;
    style_notes: string;
    director: { camera_profile: string; lens: string; movement: string; duration: number; aspect: string; audio_mode: string };
    post: { captions: boolean; music: boolean; watermark: boolean };
  };
}

export interface ProductionValues {
  brand?: string;
  project_slug?: string;
  product_name?: string;
  offer?: string;
  audience?: string;
  promise?: string;
  channels?: string[];
  product_reference_path?: string;
  logo_path?: string;
  character_id?: string;
  voice_id?: string;
  style_notes?: string;
  director?: { camera_profile?: string; lens?: string; movement?: string; duration?: number; aspect?: string; audio_mode?: string };
  post?: { captions?: boolean; music?: boolean; watermark?: boolean };
}

export interface ProductionLock {
  id: string;
  label: string;
  status: LockStatus;
  value: string | null;
  note: string;
  /** Only from `brand_check`: whether the reference file exists (null for URLs). */
  exists?: boolean | null;
}

export interface BrandKit {
  brand: string;
  readiness: 'ready' | 'needs_refs';
  locks: { product_ref: ProductionLock; logo: ProductionLock; character: ProductionLock; voice: ProductionLock };
  missing: ProductionLock[];
  warnings?: string[];
}

export interface Campaign {
  brand: string;
  project_slug: string;
  product_name: string;
  offer: string;
  audience: string;
  promise: string;
  identity_locks: BrandKit['locks'];
  required_assets: ProductionLock[];
  hooks: Array<{ id: string; label: string; angle: string; line: string }>;
  scenes: Array<{ id: string; label: string; duration_s: number; visual_role: string; prompt: string; voiceover: string; required_refs: string[] }>;
  variants: Array<{ id: string; channel: string; label: string; aspect: string; duration_s: number; caption_style: string; cta: string; priority: 'primary' | 'draft' }>;
  handoff: { image_prompt: string; video_prompt: string; tts_script: string; suggested_model: string };
}

export interface CanvasNode {
  id: string;
  type: string;
  label: string;
  status: 'ready' | 'needs_input' | 'planned';
  x: number;
  y: number;
  summary: string;
}

export interface Canvas {
  nodes: CanvasNode[];
  edges: Array<{ from: string; to: string; label: string }>;
}

export interface Director {
  subject: string;
  duration: number;
  aspect: string;
  prompt_addendum: string;
  control_stack: Array<{ label: string; value: string }>;
  checklist: string[];
  shot_contract_inline: Record<string, unknown>;
}

export interface PostPlan {
  tasks: Array<{ id: string; channel: string; label: string; aspect: string; duration_s: number; output_name: string; steps: string[] }>;
}

export interface HandoffRequest {
  view: 'image' | 'video' | 'voice';
  kind: 'image' | 'video' | 'voice';
  provider: string;
  model: string;
  prompt: string;
  params: Record<string, unknown>;
  label: string;
  shot_contract?: Record<string, unknown>;
  notes?: string;
  workflow?: Record<string, unknown>;
}

export interface HandoffEntry {
  status: HandoffStatus;
  reason: string | null;
  request: HandoffRequest;
}

export interface Handoffs {
  image: HandoffEntry;
  video: HandoffEntry;
  voice: HandoffEntry;
}

/** Versioned project document; campaign, canvas, shot plan and post plan derive from it. Gates stay empty for now. */
export interface ProjectDoc {
  schema_version: string;
  kind: 'production_project';
  project_slug: string;
  beats: Array<{ id: string; purpose: string; label?: string; script_text?: string | null; shot_contract?: Record<string, unknown> | null; voice?: unknown; music_cue?: unknown; caption?: unknown; status: string }>;
  gates: { beat_map: { approved_at?: string | null }; look: { chosen?: string | null } };
}

export interface ProductionPack {
  schema_version: string;
  kind: 'campaign_production_pack';
  project_slug: string;
  campaign: Campaign;
  canvas: Canvas;
  director: Director;
  post: PostPlan;
  handoff: Handoffs;
  project?: ProjectDoc;
}

export interface ProductionBuild {
  project: ProjectDoc;
  campaign: Campaign;
  brand_kit: BrandKit;
  canvas: Canvas;
  director: Director;
  post: PostPlan;
  handoff: Handoffs;
  pack: ProductionPack;
}

export interface PackDraft {
  draft_id: string;
  status: 'draft';
  created_at: string;
  pack_dir: string;
  manifest_path: string;
  files: string[];
  production_pack: ProductionPack;
}

export interface PreflightReport {
  ok: boolean;
  project_slug: string;
  handoff: Record<'image' | 'video' | 'voice', HandoffStatus>;
  blocked: string[];
  exports: number;
  warnings: string[];
}

export interface SavedPack {
  draft_id: string;
  project_slug: string;
  created_at: string;
  pack_dir: string;
  manifest_path: string;
}

/** Engine commands `studio.production.*` through the host's client. */
function call<T>(verb: string, input: Record<string, unknown> = {}): Promise<T> {
  return studio<T>(`production.${verb}`, input);
}

export const productionApi = {
  templates: () => call<ProductionTemplates>('templates'),
  build: (values: ProductionValues) => call<ProductionBuild>('build', values as Record<string, unknown>),
  brandCheck: (values: ProductionValues) => call<BrandKit>('brand_check', values as Record<string, unknown>),
  preflight: (pack: ProductionPack) => call<PreflightReport>('preflight', { pack }),
  savePack: (pack: ProductionPack) => call<PackDraft>('save_pack', { pack }),
  packs: () => call<{ packs: SavedPack[] }>('packs')
};
