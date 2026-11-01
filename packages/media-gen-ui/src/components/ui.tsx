import type { ReactNode } from 'react';
import { Card } from '@rightkit/react-shell/react';

export function Kv({ rows }: { rows: Array<[string, ReactNode]> }) {
  return (
    <Card flush className="kv-card">
    <dl className="kv">
      {rows.map(([k, v]) => (
        <FragmentRow key={k} k={k} v={v} />
      ))}
    </dl>
    </Card>
  );
}

function FragmentRow({ k, v }: { k: string; v: ReactNode }) {
  return (
    <>
      <dt>{k}</dt>
      <dd>{v}</dd>
    </>
  );
}

export function usd(value: number | null | undefined, digits = 2): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return 'unknown';
  if (value === 0) return 'Free';
  return `$${value.toFixed(value < 0.1 ? 3 : digits)}`;
}

export function relativeTime(iso: string | number | undefined): string {
  if (iso === undefined) return '';
  const ms = typeof iso === 'number' ? iso : Date.parse(iso);
  if (!Number.isFinite(ms)) return '';
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  const d = Math.floor(s / 86400);
  return d === 1 ? 'yesterday' : `${d} days ago`;
}

export function formatBytes(bytes: number | null | undefined): string {
  if (!bytes) return '—';
  const gb = bytes / 1e9;
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  return `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

export function baseName(path: string, fallback = 'asset'): string {
  if (path.startsWith('data:') || (import.meta.env?.DEV && path.startsWith('browser-qa-mock:'))) return fallback;
  return path.split(/[\\/]/).filter(Boolean).pop() || fallback;
}

export const PROVIDER_META: Record<string, { name: string; desc: string; placeholder: string }> = {
  openai: { name: 'OpenAI', desc: 'GPT Image 2 · TTS', placeholder: 'sk-…' },
  google: { name: 'Google AI', desc: 'Nano Banana 2 · Veo · Gemini TTS', placeholder: 'AIza…' },
  fal: { name: 'fal', desc: 'FLUX.2 images · Veo 3.1 video', placeholder: 'fal_…' },
  elevenlabs: { name: 'ElevenLabs', desc: 'Eleven v4 voices', placeholder: 'xi_…' },
  heygen: { name: 'HeyGen', desc: 'Avatar video', placeholder: 'hg_…' },
  nanobanana: { name: 'NanoBanana', desc: 'Nano Banana 2 images', placeholder: 'key' },
  wavespeed: { name: 'WaveSpeed', desc: 'Kling · Seedance · Wan video · voice · avatars', placeholder: 'ws_…' },
  higgsfield: { name: 'Higgsfield', desc: '50+ models', placeholder: 'hf_…' },
  evolink: { name: 'Evolink', desc: 'GPT Image', placeholder: 'key' },
  'local-qts': { name: 'Qwen3-TTS', desc: 'Local voice engine · no key needed', placeholder: '' }
};

export const providerName = (id: string) => PROVIDER_META[id]?.name ?? id;
