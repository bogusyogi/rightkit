import type { Brand } from './gallery.js';

export interface ImageIntent {
  model: string;
  label: string;
  use: string;
  estimated_cost: string;
}

export interface ImageStudioIntentsResponse {
  intents: Record<string, ImageIntent>;
}

export type ImageFormInputType =
  | 'dropdown'
  | 'bounded_number'
  | 'number'
  | 'toggle'
  | 'limited_text'
  | 'text'
  | 'color_list'
  | 'color'
  | 'string_list'
  | 'json_object';

export interface ImageModelFormField {
  field: string;
  type: 'string' | 'integer' | 'number' | 'boolean' | 'enum' | 'array' | 'object';
  input_type: ImageFormInputType;
  constraints: {
    enum?: string[];
    min?: number;
    max?: number;
    min_length?: number;
    max_length?: number;
    max_items?: number;
    supported?: boolean;
    examples?: string[];
  };
  default: unknown;
  required: boolean;
  format: string | null;
  note: string | null;
}

export interface ImageModelFormSchema {
  schema_version: '1.0.0';
  model: string;
  contract_version: string | null;
  inheritance_chain: string[];
  negative_prompt_policy: {
    strategy?: string;
    [key: string]: unknown;
  } | null;
  fields: ImageModelFormField[];
}

export type VideoFormInputType = ImageFormInputType;

export interface VideoModelFormField {
  field: string;
  type: 'string' | 'integer' | 'number' | 'boolean' | 'enum' | 'array' | 'object';
  input_type: VideoFormInputType;
  constraints: {
    enum?: Array<string | number>;
    min?: number;
    max?: number;
    min_length?: number;
    max_length?: number;
    max_items?: number;
    supported?: boolean;
    locked?: boolean;
    examples?: string[];
  };
  default: unknown;
  required: boolean;
  format: string | null;
  note: string | null;
}

export interface VideoModelFormSchema {
  schema_version: '1.0.0';
  model: string;
  contract_version: string | null;
  source: string | null;
  generation_param_fields: string[];
  cost: Record<string, unknown> | null;
  fields: VideoModelFormField[];
}

export interface ImageStudioReference {
  path: string;
  label: string;
  source: 'content_reference' | 'codex_generated';
  project_slug: string;
  brand: Brand;
}

export interface ImageStudioReferencesResponse {
  references: ImageStudioReference[];
  reference_cap: number;
  roots: string[];
  diagnostics: Array<{
    severity: string;
    path?: string;
    message: string;
  }>;
}

export interface ProviderCreditSummary {
  provider: string;
  label: string;
  mode: 'live' | 'unavailable' | 'reactive_only';
  available: boolean;
  balance: number | null;
  unit: string | null;
  endpoint: string | null;
  source_url: string | null;
  status_label: string;
  error: string | null;
  balances?: Record<string, unknown>;
}

export interface ProviderCreditsResponse {
  schema_version: '1.0.0';
  generated_at: string;
  providers: ProviderCreditSummary[];
}
