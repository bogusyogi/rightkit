/**
 * Browser-mode stand-in for the `studio.production.*` engine commands, so the
 * view can be driven and screenshotted without the Tauri sidecar. The Rust
 * module (crates/genright-studio/src/production.rs) is the product; this
 * mirrors its response shapes and reuses the baseline TS builders for the
 * campaign, canvas, director and post plans. It never writes to disk.
 */
import templates from '../studio-data/production/templates.json' with { type: "json" };
import {
  buildCampaignFactoryPlan,
  buildDirectorShotPlan,
  buildPostProductionPlan,
  buildProductionCanvas,
  summarizeBrandKit,
  type CampaignChannel
} from './production-stack-builders.js';
import type { Brand } from './types.js';
import type { ProductionValues } from './production-api.js';

type Obj = Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any

function kitInput(v: ProductionValues) {
  return {
    brand: (v.brand || templates.default_brand) as Brand,
    projectSlug: v.project_slug,
    productName: v.product_name,
    offer: v.offer,
    audience: v.audience,
    promise: v.promise,
    channels: v.channels as CampaignChannel[] | undefined,
    productReferencePath: v.product_reference_path,
    logoPath: v.logo_path,
    characterId: v.character_id,
    voiceId: v.voice_id,
    styleNotes: v.style_notes
  };
}

function nearest(aspect: string, allowed: string[]): string {
  if (allowed.includes(aspect)) return aspect;
  const ratio = (s: string) => {
    const [w, h] = s.split(':').map(Number);
    return Math.log(w / h);
  };
  const t = ratio(aspect);
  return allowed.reduce((best, a) => (Math.abs(ratio(a) - t) < Math.abs(ratio(best) - t) - 1e-9 ? a : best), allowed[0]);
}

function handoff(campaign: Obj, director: Obj): Obj {
  const h = templates.handoff;
  const ref: string | null = campaign.identity_locks.product_ref.value;
  const primary = campaign.variants[0];
  const aspect = primary?.aspect || director.aspect;
  const slug = campaign.project_slug;
  const fill = (s: string, vars: Record<string, string>) => s.replace(/\{(\w+)\}/g, (_m, k) => vars[k] ?? '');

  const imgAspect = nearest(aspect, h.image.aspects);
  const imgReason = !ref ? h.reasons.image_no_ref : imgAspect !== aspect ? fill(h.reasons.image_aspect, { aspect, sent: imgAspect }) : null;
  const vidAspect = nearest(aspect, h.video.aspects);
  const duration = Math.round(Math.max(h.video.min_duration_s, Math.min(h.video.max_duration_s, primary?.duration_s ?? director.duration)));
  const vidStatus = !ref ? 'blocked' : vidAspect !== aspect ? 'ready_with_warnings' : 'ready';
  const vidReason = !ref ? h.reasons.video_no_ref : vidAspect !== aspect ? fill(h.reasons.video_aspect, { aspect, sent: vidAspect }) : null;
  const script: string = campaign.handoff.tts_script;
  const voiceId: string | null = campaign.identity_locks.voice.value;
  return {
    image: {
      status: imgReason ? 'ready_with_warnings' : 'ready',
      reason: imgReason,
      request: {
        view: 'image', kind: 'image', provider: h.image.provider, model: h.image.model, prompt: campaign.handoff.image_prompt,
        params: { aspect: imgAspect, resolution: h.image.resolution, ...(ref ? { image_urls: [ref] } : {}) },
        label: `${slug}-hero-frame`, notes: h.image.notes, workflow: { kind: 'campaign_hero_frame', required_for_video: true }
      }
    },
    video: {
      status: vidStatus,
      reason: vidReason,
      request: {
        view: 'video', kind: 'video', provider: h.video.provider, model: h.video.model,
        prompt: `${campaign.handoff.video_prompt} ${director.prompt_addendum}`.trim(),
        params: { aspect: vidAspect, resolution: h.video.resolution, duration, ...(ref ? { image_url: ref } : {}) },
        label: `${slug}-video`,
        shot_contract: {
          ...director.shot_contract_inline,
          source_frame_read: ref ? h.source_frame_read_ok : h.source_frame_read_missing,
          campaign_scenes: campaign.scenes.map((s: Obj) => ({ id: s.id, label: s.label, visual_role: s.visual_role, duration_s: s.duration_s, prompt: s.prompt })),
          campaign_variants: campaign.variants.map((v: Obj) => ({ channel: v.channel, aspect: v.aspect, duration_s: v.duration_s, caption_style: v.caption_style }))
        },
        workflow: { kind: 'campaign_video', source: 'genright_campaign_stack' }
      }
    },
    voice: {
      status: script ? 'ready' : 'blocked',
      reason: script ? null : h.reasons.voice_no_script,
      request: {
        view: 'voice', kind: 'voice', provider: h.voice.provider, model: h.voice.model, prompt: script,
        params: voiceId ? { voice_id: voiceId } : {}, label: `${slug}-voice`, workflow: { kind: 'campaign_voice' }
      }
    }
  };
}

function build(v: ProductionValues): Obj {
  const campaign = buildCampaignFactoryPlan(kitInput(v)) as unknown as Obj;
  const brandKit = summarizeBrandKit({ ...kitInput(v), productName: campaign.product_name, offer: campaign.offer }) as unknown as Obj;
  const d = v.director ?? {};
  const director = buildDirectorShotPlan({
    subject: campaign.product_name,
    duration: d.duration,
    aspect: d.aspect,
    cameraProfile: d.camera_profile,
    lens: d.lens,
    movement: d.movement,
    audioMode: d.audio_mode as 'silent' | 'voiceover' | 'music' | undefined,
    continuityLocks: (v.style_notes ?? '').split(',').map((s) => s.trim()).filter(Boolean)
  }) as unknown as Obj;
  const post = buildPostProductionPlan(campaign as never, { captions: v.post?.captions, music: v.post?.music, watermark: v.post?.watermark }) as unknown as Obj;
  const canvas = buildProductionCanvas(campaign as never) as unknown as Obj;
  const ho = handoff(campaign, director);
  const project = {
    schema_version: '1.0.0', kind: 'production_project', project_slug: campaign.project_slug, brand: campaign.brand,
    brief: { product_name: campaign.product_name, offer: campaign.offer, audience: campaign.audience, promise: campaign.promise },
    beats: campaign.scenes.map((s: Obj) => ({ id: s.id, purpose: s.visual_role, label: s.label, script_text: s.voiceover, shot_contract: { prompt: s.prompt, duration_s: s.duration_s, required_refs: s.required_refs }, voice: null, music_cue: null, caption: null, status: 'planned' })),
    gates: { beat_map: { approved_at: null }, look: { chosen: null } },
    director
  };
  const pack = { schema_version: '1.0.0', kind: 'campaign_production_pack', project_slug: campaign.project_slug, created_source: 'genright_campaign_stack', campaign, canvas, director, post, handoff: ho, project };
  return { project, campaign, brand_kit: brandKit, canvas, director, post, handoff: ho, pack };
}

/** Packs saved this browser session, mirroring the engine's persisted list. */
const savedPacks: Obj[] = [];

export async function mockProduction(verb: string, input: Record<string, unknown>): Promise<unknown> {
  const v = input as ProductionValues;
  switch (verb) {
    case 'templates':
      return templates;
    case 'build':
      return build(v);
    case 'brand_check': {
      const kit = summarizeBrandKit({ ...kitInput(v), productName: v.product_name, offer: v.offer }) as unknown as Obj;
      return { ...kit, warnings: [] };
    }
    case 'preflight': {
      const pack = (input.pack ?? {}) as Obj;
      if (pack.kind !== 'campaign_production_pack') throw Object.assign(new Error('invalid_input: Payload must be a campaign production pack'), { code: 'invalid_input' });
      const statuses = Object.fromEntries(['image', 'video', 'voice'].map((k) => [k, pack.handoff[k].status]));
      const warnings = ['image', 'video', 'voice'].filter((k) => pack.handoff[k].reason).map((k) => `${k}: ${pack.handoff[k].reason}`);
      return { ok: true, project_slug: pack.project_slug, handoff: statuses, blocked: Object.keys(statuses).filter((k) => statuses[k] === 'blocked'), exports: pack.post.tasks.length, warnings };
    }
    case 'save_pack': {
      const pack = (input.pack ?? {}) as Obj;
      if (pack.kind !== 'campaign_production_pack') throw Object.assign(new Error('invalid_input: Payload must be a campaign production pack'), { code: 'invalid_input' });
      const id = `production-${new Date().toISOString().replace(/\D/g, '').slice(0, 14)}-${pack.project_slug}-mock0001`;
      const created_at = new Date().toISOString();
      const pack_dir = `~/Library/Application Support/App/packs/${id}`;
      savedPacks.unshift({ draft_id: id, project_slug: pack.project_slug, created_at, pack_dir, manifest_path: `${pack_dir}/manifest.json` });
      return {
        draft_id: id, status: 'draft', created_at, production_pack: pack,
        pack_dir,
        manifest_path: `${pack_dir}/manifest.json`,
        files: ['project.json', 'pack.json', 'campaign.json', 'canvas.json', 'shot_contract.json', 'post_plan.json', 'handoff_image.json', 'handoff_video.json', 'handoff_voice.json', 'manifest.json']
      };
    }
    case 'packs':
      return { packs: savedPacks };
    default:
      throw new Error(`unsupported production mock command ${verb}`);
  }
}
