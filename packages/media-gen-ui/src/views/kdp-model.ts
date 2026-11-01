// Pure helpers and engine types for the KDP view (see crates/genright-studio/src/kdp.rs).

export interface KdpStyle { id: string; label: string; prompt_fragment: string }

export interface KdpStyles {
  styles: KdpStyle[];
  default_style: string;
  title: string;
  prompt_label: string;
  locks: { provider: string; model: string; model_label: string; aspect: string; resolution: string; brand: string };
  estimate: { credits: number; usd: number };
}

export interface KdpSource {
  path: string;
  basename: string;
  project_slug: string;
  source: 'kdp' | 'codex_generated';
  size_bytes: number;
  modified_ms?: number;
  /** Browser-mode previews only; the desktop app loads `path` through the asset protocol. */
  thumb?: string;
}

export interface KdpSources {
  references: KdpSource[];
  reference_cap: number;
  roots: string[];
  diagnostics: Array<{ severity: string; message: string }>;
}

export interface KdpRoot { path: string; kind: 'kdp' | 'codex'; exists?: boolean }
export interface KdpSettings { roots: KdpRoot[]; customized: boolean; defaults: KdpRoot[] }

export interface KdpDraft {
  status: string;
  est_usd?: number;
  request: { kind: string; provider: string; model: string; prompt: string; params: Record<string, unknown> };
  workflow: { style: string; style_label: string; project_slug: string; source_path: string };
  job?: { job_id: string; status: string; est_usd: number };
  /** Preflight only: the page is a local file, so the run needs `confirm_fal_upload: true`. */
  requires_fal_upload_confirmation?: boolean;
  local_references?: string[];
}

export function canRun(sourcePath: string, running: boolean): boolean {
  return Boolean(sourcePath) && !running;
}

export function compactSource(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.length <= 3 ? parts.join('/') : `…/${parts.slice(-3).join('/')}`;
}

export function sourceSummary(count: number): string {
  return count === 1 ? '1 KDP image found' : `${count} KDP images found`;
}

export function estimateText(usd: number | undefined, credits: number | undefined): string {
  if (usd == null) return '—';
  return credits != null ? `$${usd.toFixed(2)} · ${credits} credits` : `$${usd.toFixed(2)}`;
}

/** Insert or drop a source root; the path is trimmed and a duplicate is ignored. */
export function addRoot(roots: KdpRoot[], path: string, kind: KdpRoot['kind']): KdpRoot[] {
  const p = path.trim();
  if (!p || roots.some((r) => r.path === p)) return roots;
  return [...roots, { path: p, kind }];
}

export function removeRoot(roots: KdpRoot[], path: string): KdpRoot[] {
  return roots.filter((r) => r.path !== path);
}
