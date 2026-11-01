// Asset facts, scope, library search and asset import, as the engine returns them.
// Field names follow docs/ENGINE-CLI.md ("asset import", "library search") and
// crates/genright-e2e/tests/fast_asset_facts.rs. The three facts are never merged into one status.

export type AssetOriginKind = 'unknown' | 'generated' | 'captured' | 'acquired' | 'supplied';
export type AssetEvidence = 'unknown' | 'authentic' | 'illustrative';
export type AssetRightsStatus = 'unknown' | 'unresolved' | 'cleared';

export interface AssetOrigin {
  kind: AssetOriginKind;
  provider: string | null;
  model: string | null;
  job_id: string | null;
  /** Where an acquired or supplied asset came from (a URL, path or supplier). */
  source: string | null;
  /** Parent asset ids of a derived asset. */
  parents: string[];
}

export interface AssetRights {
  status: AssetRightsStatus;
  licence_evidence: string[];
  source: string | null;
  attribution: string | null;
  permitted_uses: string[];
  consent_refs: string[];
}

export interface AssetFacts {
  origin: AssetOrigin;
  evidence: AssetEvidence;
  rights: AssetRights;
}

/** The content-brief channel object: venture, channel and account ids. */
export interface AssetScope {
  venture: string;
  channel: string;
  account: string;
}

export interface LibraryHit {
  asset_id: string;
  job_id: string;
  path: string;
  exists: boolean;
  kind: string;
  provider: string | null;
  model: string | null;
  /** FTS5 snippet; matches are wrapped in [brackets]. */
  prompt_snippet: string | null;
  facts: AssetFacts | null;
  scope: AssetScope | null;
}

export interface LibrarySearchFilters {
  kind?: string;
  provider?: string;
  origin?: AssetOriginKind;
  rights_status?: AssetRightsStatus;
  venture?: string;
  channel?: string;
  account?: string;
}

export interface LibrarySearchRequest {
  query: string;
  limit?: number;
  cursor?: string;
  filters?: LibrarySearchFilters;
}

export interface LibrarySearchResult {
  query: string;
  total: number;
  limit: number;
  filters: LibrarySearchFilters;
  hits: LibraryHit[];
  next_cursor: string | null;
  index: { assets: number; refreshed: boolean; rebuilt: boolean };
  not_indexed: string[];
}

export type ImportOrigin = 'supplied' | 'acquired' | 'captured';
export type ImportEvidence = 'authentic' | 'illustrative' | 'unknown';
export type ImportRightsStatus = 'unknown' | 'unresolved' | 'cleared';

export interface AssetImportRights {
  status?: ImportRightsStatus;
  licence_evidence?: string[];
  attribution?: string;
}

export interface AssetImportRequest {
  path: string;
  origin: ImportOrigin;
  evidence?: ImportEvidence;
  rights?: AssetImportRights;
  scope?: AssetScope;
  dry_run?: boolean;
}

/** Result of a dry run: what a real import would record. Nothing is written. */
export interface AssetImportDryRun {
  dry_run: true;
  would_import: {
    path: string;
    sha256?: string;
    bytes?: number;
    mime?: string;
    facts: AssetFacts;
    scope: AssetScope | null;
  };
}

export interface AssetImportResult {
  asset_id: string;
  job_id: string;
  path: string;
  sha256: string;
  bytes: number;
  mime: string;
  exists: boolean;
  facts: AssetFacts;
  scope: AssetScope | null;
  source_preserved: boolean;
}
