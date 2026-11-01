import type { ModelEntry, ModelsBackend } from '@rightkit/onboarding';
import { fetchLocalImageProfiles, installLocalImageProfile, pauseLocalImageInstall } from './api.js';
import type { LocalImageProfile } from './types.js';

function progressOf(p: LocalImageProfile): number {
  if (p.install_state === 'installed') return 1;
  const comps = p.components.filter((c) => c.required);
  if (comps.length === 0) return p.install_state === 'installing' ? 0.1 : 0;
  const score = comps.reduce((n, c) => n + (c.state === 'installed' ? 1 : c.state === 'verifying' || c.state === 'staged' ? 0.7 : 0), 0);
  return score / comps.length;
}

function toEntry(p: LocalImageProfile): ModelEntry {
  const state: ModelEntry['state'] = p.install_state === 'installed' ? 'installed'
    : p.install_state === 'installing' ? 'downloading'
    : p.install_state === 'failed' ? 'error'
    : 'missing';
  return {
    id: p.id,
    label: p.label,
    description: `${p.description} · ${p.license.name}`,
    sizeBytes: p.total_bytes,
    state,
    progress: state === 'downloading' ? progressOf(p) : undefined,
    error: state === 'error' ? { code: 'install_failed', message: 'Last install failed' } : undefined,
  };
}

/**
 * RightKit ModelsBackend over the host app local image-model install API. `cancel` is the Pause
 * control: it pauses the `rightkit-models` install (the partial download is kept and installing
 * again resumes it); nothing here discards a partial.
 * Progress is polled because the engine reports it through the profile list rather than
 * pushing events.
 */
export function createLocalModelsBackend(onChange?: () => void): ModelsBackend {
  const list = async () => (await fetchLocalImageProfiles()).profiles.map(toEntry);
  return {
    list,
    async install(id) {
      await installLocalImageProfile(id);
      onChange?.();
    },
    async cancel(id) {
      await pauseLocalImageInstall(id);
      onChange?.();
    },
    async subscribe(handler) {
      const seen = new Map<string, string>();
      const tick = async () => {
        try {
          for (const e of await list()) {
            const sig = `${e.state}:${Math.round((e.progress ?? 0) * 100)}`;
            const prev = seen.get(e.id);
            seen.set(e.id, sig);
            if (prev !== undefined && prev !== sig) {
              handler(e);
              if (e.state === 'installed' || e.state === 'error') onChange?.();
            }
          }
        } catch {
          /* next tick retries */
        }
      };
      const t = window.setInterval(() => void tick(), 1500);
      return () => window.clearInterval(t);
    },
  };
}
