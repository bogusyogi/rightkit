import type { Brand } from './gallery';

export type WorkflowStatus = 'runnable' | 'advanced' | 'planned' | 'guarded';

export interface WorkflowCatalogItem {
  id: string;
  label: string;
  status: WorkflowStatus;
  model_slug: string;
  description: string;
  locked_settings: Record<string, string | number | boolean>;
  operator_choices: string[];
  entry: string;
  route_options?: Array<{
    id: string;
    label: string;
    intent: string;
    model_slug: string;
    locked_settings?: Record<string, string | number | boolean>;
  }>;
}

export interface WorkflowCatalogModel {
  slug: string;
  backend: string;
  ref_kind: string;
  frame_roles: string[];
}

export interface WorkflowCatalogGroup {
  id: string;
  label: string;
  provider: string;
  model_slugs?: string[];
  route_intents?: string[];
  best_for: string[];
  avoid_for?: string[];
  prompt_rules: string[];
  workflows: WorkflowCatalogItem[];
  models?: WorkflowCatalogModel[];
}

export interface WorkflowCatalogSection {
  id: 'image' | 'video';
  label: string;
  groups: WorkflowCatalogGroup[];
}

export interface WorkflowCatalogResponse {
  schema_version: '1.0.0';
  sources: string[];
  sections: WorkflowCatalogSection[];
}

export interface StudioCharacter {
  id: string;
  character_id: string;
  venture: string;
  project_slug: string;
  brand: Brand;
  description: string;
  has_lora: boolean;
  lora_base_model: string | null;
  refs_count: number;
  identifiable: boolean;
}

export interface StudioCharactersResponse {
  characters: StudioCharacter[];
  root: string;
  diagnostics: Array<{
    severity: string;
    path?: string;
    message: string;
  }>;
}

export interface AestheticLora {
  id: string;
  name: string;
  description: string;
  source_url: string;
  license: string;
  commercial_ok: boolean;
  base_model: string;
  default_scale: number;
  vetting_status: string;
  source_verified_at: string;
  test_renders: unknown[];
}

export interface AestheticLorasResponse {
  aesthetic_loras: AestheticLora[];
  diagnostics: Array<{
    severity: string;
    path?: string;
    message: string;
  }>;
}

export interface VideoRecipe {
  slug: string;
  name: string;
  prompt_fragment: string;
  category?: string;
  description?: string;
  [key: string]: unknown;
}

