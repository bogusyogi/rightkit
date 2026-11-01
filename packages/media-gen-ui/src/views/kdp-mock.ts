// Browser-mode (no Tauri) stand-in for the studio.kdp.* engine commands, so the
// view can be exercised in QA. Mirrors the shapes in crates/genright-studio/src/kdp.rs.
import type { KdpDraft, KdpSettings, KdpSources, KdpStyles } from './kdp-model.js';

/** Submits the built request to the (mock) engine; injected so this file stays dependency-free. */
export type KdpSubmit = (request: Record<string, unknown>) => Promise<{ job_id: string; status: string; est_usd: number }>;

const STYLES: KdpStyles = {
  styles: [
    { id: 'crayon', label: 'Crayon', prompt_fragment: 'Apply a soft wax crayon look.' },
    { id: 'marker', label: 'Marker', prompt_fragment: 'Apply a clean alcohol-marker coloring look.' },
    { id: 'colored_pencil', label: 'Colored pencil', prompt_fragment: 'Apply a polished colored pencil look.' }
  ],
  default_style: 'colored_pencil',
  title: 'Color thumbnail',
  prompt_label: 'KDP colorize',
  locks: { provider: 'nanobanana', model: 'nano-banana-2', model_label: 'NB2', aspect: '1:1', resolution: '2K', brand: 'KDP' },
  estimate: { credits: 12, usd: 0.06 }
};

function lineArt(seed: number): string {
  const r = 38 + (seed % 5) * 6;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200"><rect width="200" height="200" fill="#fff"/><g fill="none" stroke="#111" stroke-width="3" stroke-linejoin="round"><circle cx="100" cy="92" r="${r}"/><circle cx="82" cy="84" r="6"/><circle cx="118" cy="84" r="6"/><path d="M80 110q20 18 40 0"/><path d="M${100 - r} 60q-20-30 4-40 14 18 20 28"/><path d="M${100 + r} 60q20-30-4-40-14 18-20 28"/><path d="M60 170q40-30 80 0"/></g></svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

const NAMES: Array<[string, string]> = [
  ['book-1-ganesha', 'page-01.png'], ['book-1-ganesha', 'page-02.png'], ['book-1-ganesha', 'page-03.png'],
  ['book-2-rama', 'page-01.png'], ['book-2-rama', 'page-07.png'], ['book-3-hanuman', 'page-04.png']
];

const SOURCES: KdpSources = {
  references: NAMES.map(([project, file], i) => ({
    path: `/Users/you/KDP/${project}/${file}`,
    basename: file,
    project_slug: project,
    source: 'kdp' as const,
    size_bytes: 180_000 + i * 9_000,
    modified_ms: Date.now() - i * 3_600_000,
    thumb: lineArt(i)
  })),
  reference_cap: 200,
  roots: ['/Users/you/KDP'],
  diagnostics: []
};

let settings: KdpSettings = {
  roots: [{ path: '/Users/you/KDP', kind: 'kdp', exists: true }],
  customized: false,
  defaults: [{ path: '/Users/you/KDP', kind: 'kdp' }]
};

export async function mockKdp<T>(name: string, input: Record<string, unknown>, submit?: KdpSubmit): Promise<T> {
  switch (name) {
    case 'studio.kdp.styles':
      return STYLES as T;
    case 'studio.kdp.sources':
      return SOURCES as T;
    case 'studio.kdp.settings':
      return settings as T;
    case 'studio.kdp.settings_set': {
      const roots = Array.isArray(input.roots) ? (input.roots as KdpSettings['roots']) : null;
      settings = roots
        ? { ...settings, roots: roots.map((r) => ({ ...r, exists: true })), customized: true }
        : { ...settings, roots: settings.defaults.map((r) => ({ ...r, exists: true })), customized: false };
      return settings as T;
    }
    case 'studio.kdp.preflight':
    case 'studio.kdp.run': {
      const style = STYLES.styles.find((s) => s.id === (input.style ?? STYLES.default_style));
      if (!style) throw new Error(`invalid_input: Unsupported KDP thumbnail style "${String(input.style)}"`);
      const request = {
        kind: 'image',
        provider: STYLES.locks.provider,
        model: STYLES.locks.model,
        prompt: `Colorize the selected KDP black-and-white coloring page. ${style.prompt_fragment}`,
        params: { aspect: '1:1', resolution: '2K', image_urls: [String(input.source_path)] }
      };
      const draft: KdpDraft = {
        status: 'draft',
        est_usd: 0.06,
        request,
        workflow: { style: style.id, style_label: style.label, project_slug: String(input.project_slug || 'kdp-color-thumbnail'), source_path: String(input.source_path) }
      };
      if (name === 'studio.kdp.run') {
        if (!submit) throw new Error('unavailable: no engine to submit to');
        // Per-run consent is forwarded only when it is literally `true`, as the engine does.
        const job = await submit({ ...request, request_id: String(input.request_id ?? '') || undefined, ...(input.confirm_fal_upload === true ? { confirm_fal_upload: true } : {}) });
        return { ...draft, status: 'submitted', job } as T;
      }
      // The page is local and NanoBanana reads only URLs: the run needs `confirm_fal_upload: true`.
      return { ...draft, requires_fal_upload_confirmation: true, local_references: [String(input.source_path)] } as T;
    }
    default:
      throw new Error(`unknown_command: ${name}`);
  }
}
