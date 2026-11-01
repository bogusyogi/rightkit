// Pure filter logic for the Gallery view (baseline: src/components/FilterBar.tsx + App.tsx
// `matchesFilter`). No React or DOM here so node tests can import it.

export const ALL = 'all';

export const EYES_STATES = ['unknown', 'pending', 'approved', 'rejected', 'rework', 'stale'] as const;
export const JURY_STATES = ['unknown', 'pending', 'passed', 'blocked', 'warn_only'] as const;
export const METADATA_STATES = ['complete', 'incomplete', 'malformed', 'missing_media'] as const;

export interface BrandInfo {
  code: string;
  name: string;
  descriptor: string;
  name_verified?: boolean;
}

export interface FilterFields {
  type: 'image' | 'video' | 'audio';
  project: string;
  brand: string;
  eyes: string;
  jury: string;
  metadata: string;
  /** Slug of the engine caller that registered the file ("postright"); empty when none did. */
  caller: string;
  /** Lowercased text the search box matches against. */
  haystack: string;
}

export interface GalleryFilters {
  query: string;
  type: 'all' | 'image' | 'video' | 'audio';
  brand: string;
  eyes: string;
  jury: string;
  project: string;
  metadata: string;
  /** Caller slug ("postright") or `all`. */
  caller: string;
}

export const NO_FILTERS: GalleryFilters = { query: '', type: 'all', brand: ALL, eyes: ALL, jury: ALL, project: ALL, metadata: ALL, caller: ALL };

/**
 * The project filter value for "No project" (library assets). It is the empty slug, which no
 * project folder can have, and `serverFilters` sends it as `no_project: true`, never as `project: ''`.
 */
export const NO_PROJECT_FILTER = '';

const pick = (value: string, wanted: string) => wanted === ALL || value === wanted;

export function matchesFilters(item: FilterFields, f: GalleryFilters): boolean {
  const q = f.query.trim().toLowerCase();
  return (
    (f.type === 'all' || item.type === f.type) &&
    pick(item.brand, f.brand) &&
    pick(item.eyes, f.eyes) &&
    pick(item.jury, f.jury) &&
    pick(item.project, f.project) &&
    pick(item.metadata, f.metadata) &&
    pick(item.caller ?? '', f.caller) &&
    (!q || item.haystack.includes(q))
  );
}

/** The filters the engine applies before paging (project, brand, eyes, jury, caller); `all` is omitted. */
export function serverFilters(f: GalleryFilters): { project?: string; no_project?: boolean; brand?: string; eyes?: string; jury?: string; caller?: string } {
  const out: { project?: string; no_project?: boolean; brand?: string; eyes?: string; jury?: string; caller?: string } = {};
  if (f.project === NO_PROJECT_FILTER) out.no_project = true;
  else if (f.project !== ALL) out.project = f.project;
  if (f.brand !== ALL) out.brand = f.brand;
  if (f.eyes !== ALL) out.eyes = f.eyes;
  if (f.jury !== ALL) out.jury = f.jury;
  if (f.caller !== ALL) out.caller = f.caller;
  return out;
}

export function activeFilterCount(f: GalleryFilters): number {
  return (['brand', 'eyes', 'jury', 'project', 'metadata', 'caller'] as const).filter((k) => f[k] !== ALL).length + (f.type !== 'all' ? 1 : 0) + (f.query.trim() ? 1 : 0);
}

export interface Option {
  value: string;
  label: string;
  count: number;
}

/** Fixed options first (even at zero), then any other value present, with live counts. */
export function optionsFor(values: string[], fixed: readonly string[], label: (v: string) => string = (v) => v): Option[] {
  const counts = new Map<string, number>();
  for (const v of values) counts.set(v, (counts.get(v) ?? 0) + 1);
  const extra = [...counts.keys()].filter((v) => !fixed.includes(v)).sort((a, b) => a.localeCompare(b));
  return [...fixed, ...extra].map((v) => ({ value: v, label: label(v), count: counts.get(v) ?? 0 }));
}

/** "From the host app": the label of a caller filter option, from the label the file's caller sidecar recorded. */
export function callerLabel(slug: string, labels: ReadonlyMap<string, string>): string {
  return `From ${labels.get(slug) ?? slug}`;
}

/** "DD · Damned Designs"; unknown codes fall back to the bare code. */
export function brandLabel(code: string, brands: BrandInfo[]): string {
  const b = brands.find((x) => x.code === code);
  return b && b.name !== b.code ? `${b.code} · ${b.name}` : code;
}

export function humanize(state: string): string {
  return state.replace(/_/g, ' ');
}

/** Display text for a review state. `stale` says why an approval no longer counts. */
export function stateLabel(state: string): string {
  return state === 'stale' ? 'stale (edited after approval)' : humanize(state);
}

/** Library assets have no project, so the engine sends an empty `project_slug` for them. */
export const NO_PROJECT_LABEL = 'No project';

/** Project text for display; an empty project shows as "No project". */
export function projectLabel(project: string): string {
  return project.trim() ? project : NO_PROJECT_LABEL;
}

/**
 * The items a dropdown's options are computed from. While a server-side filter narrows the
 * loaded listing, the options come from the last unfiltered listing so the choices do not shrink.
 */
export function optionSourceItems<T>(loaded: T[], unfiltered: T[], serverFiltered: boolean): T[] {
  return serverFiltered ? unfiltered : loaded;
}

export type Tone = 'ok' | 'bad' | 'warn' | undefined;

/** Chip colour for an eyes/jury/overall/metadata state. */
export function stateTone(state: string): Tone {
  switch (state) {
    case 'approved':
    case 'passed':
    case 'complete':
    case 'exported':
      return 'ok';
    case 'rejected':
    case 'blocked':
    case 'malformed':
    case 'missing_media':
      return 'bad';
    case 'rework':
    case 'stale':
    case 'warn_only':
    case 'needs_review':
    case 'incomplete':
      return 'warn';
    default:
      return undefined;
  }
}
