// Reference only, deleted at QA close.
import type { Brand, ImageStudioRequest, VideoStudioPreflightRequest } from './types.js';

export type CampaignChannel =
  | 'tiktok'
  | 'instagram-reels'
  | 'youtube-shorts'
  | 'meta-feed'
  | 'meta-story'
  | 'linkedin-feed';

export type LockStatus = 'ready' | 'missing' | 'optional';
export type BrandKitReadiness = 'ready' | 'needs_refs';

export interface ProductionStackValues {
  brand?: Brand;
  projectSlug?: string;
  productName?: string;
  offer?: string;
  audience?: string;
  promise?: string;
  channels?: CampaignChannel[];
  productReferencePath?: string;
  logoPath?: string;
  characterId?: string;
  voiceId?: string;
  styleNotes?: string;
}

export interface ProductionLock {
  id: string;
  label: string;
  status: LockStatus;
  value: string | null;
  note: string;
}

export interface BrandKitSummary {
  brand: Brand;
  readiness: BrandKitReadiness;
  locks: {
    product_ref: ProductionLock;
    logo: ProductionLock;
    character: ProductionLock;
    voice: ProductionLock;
  };
  missing: ProductionLock[];
}

export interface CampaignHook {
  id: string;
  label: string;
  angle: string;
  line: string;
}

export interface CampaignScene {
  id: string;
  label: string;
  duration_s: number;
  visual_role: string;
  prompt: string;
  voiceover: string;
  required_refs: string[];
}

export interface CampaignVariant {
  id: string;
  channel: CampaignChannel;
  label: string;
  aspect: string;
  duration_s: number;
  caption_style: string;
  cta: string;
  priority: 'draft' | 'primary';
}

export interface CampaignFactoryPlan {
  schema_version: '1.0.0';
  brand: Brand;
  project_slug: string;
  product_name: string;
  offer: string;
  audience: string;
  promise: string;
  identity_locks: BrandKitSummary['locks'];
  required_assets: ProductionLock[];
  hooks: CampaignHook[];
  scenes: CampaignScene[];
  variants: CampaignVariant[];
  handoff: {
    recommended_workspace: 'video-studio';
    suggested_model: 'seedance-2-fast';
    image_prompt: string;
    video_prompt: string;
    tts_script: string;
  };
}

export interface CanvasNode {
  id: string;
  type: 'brief' | 'brand_kit' | 'identity' | 'reference' | 'scene' | 'voice' | 'video' | 'post' | 'export';
  label: string;
  status: 'ready' | 'needs_input' | 'planned';
  x: number;
  y: number;
  summary: string;
}

export interface CanvasEdge {
  from: string;
  to: string;
  label: string;
}

export interface ProductionCanvas {
  schema_version: '1.0.0';
  project_slug: string;
  nodes: CanvasNode[];
  edges: CanvasEdge[];
}

export interface DirectorShotValues {
  subject?: string;
  duration?: number;
  aspect?: string;
  cameraProfile?: string;
  lens?: string;
  movement?: string;
  continuityLocks?: string[];
  audioMode?: 'silent' | 'voiceover' | 'music';
}

export interface DirectorShotPlan {
  schema_version: '1.0.0';
  subject: string;
  duration: number;
  aspect: string;
  prompt_addendum: string;
  control_stack: Array<{
    label: string;
    value: string;
  }>;
  checklist: string[];
  shot_contract_inline: {
    primary_action: string;
    subject_focus: string;
    camera_profile: string;
    lens_lock: string;
    continuity_locks: string[];
    motion_budget: string;
    lighting_lock: string;
    audio_lock: {
      mode: 'silent' | 'voiceover' | 'music';
    };
    time_segments: Array<{
      start_s: number;
      end_s: number;
      camera: string;
      action: string;
      forbidden_zone: string;
    }>;
    negative_prompt: string[];
  };
}

export interface PostProductionOptions {
  captions?: boolean;
  music?: boolean;
  watermark?: boolean;
}

export interface PostProductionPlan {
  schema_version: '1.0.0';
  project_slug: string;
  tasks: Array<{
    id: string;
    channel: CampaignChannel;
    label: string;
    aspect: string;
    duration_s: number;
    output_name: string;
    steps: string[];
  }>;
}

export type HandoffStatus = 'ready' | 'ready_with_warnings' | 'blocked';

export interface ProductionHandoffRequests {
  image: {
    status: HandoffStatus;
    reason: string | null;
    request: ImageStudioRequest;
  };
  video: {
    status: HandoffStatus;
    reason: string | null;
    request: VideoStudioPreflightRequest;
  };
  voice: {
    status: HandoffStatus;
    reason: string | null;
    request: VoiceHandoffRequest;
  };
}

/** The Voice view's hand-off request: same shape the engine's production pack emits. */
export interface VoiceHandoffRequest {
  view: 'voice';
  kind: 'voice';
  provider: string;
  model: string;
  prompt: string;
  params: Record<string, unknown>;
  label: string;
  workflow: Record<string, unknown>;
}

export interface ProductionPack {
  schema_version: '1.0.0';
  kind: 'campaign_production_pack';
  project_slug: string;
  created_source: 'genright_campaign_stack';
  campaign: CampaignFactoryPlan;
  canvas: ProductionCanvas;
  director: DirectorShotPlan;
  post: PostProductionPlan;
  handoff: ProductionHandoffRequests;
}

export interface ProductionStackDraft {
  schema_version: '1.0.0';
  draft_id: string;
  status: 'draft';
  execution_mode: 'production_pack_preflight';
  created_at: string;
  production_pack: ProductionPack;
  route: {
    slug: 'campaign-production-pack';
    provider: 'genright';
    label: string;
    use: string;
  };
  pipeline: {
    type: 'campaign_production_pack';
    execution_note: string;
  };
  manifest_path: string;
}

const DEFAULT_BRAND: Brand = 'DD';
const DEFAULT_CHANNELS: CampaignChannel[] = ['instagram-reels', 'meta-feed'];

const CHANNEL_SPECS: Record<CampaignChannel, {
  label: string;
  aspect: string;
  duration_s: number;
  caption_style: string;
  cta: string;
}> = {
  tiktok: {
    label: 'TikTok',
    aspect: '9:16',
    duration_s: 9,
    caption_style: 'native short punch captions',
    cta: 'Tap through'
  },
  'instagram-reels': {
    label: 'Instagram Reels',
    aspect: '9:16',
    duration_s: 10,
    caption_style: 'clean creator captions',
    cta: 'Save this'
  },
  'youtube-shorts': {
    label: 'YouTube Shorts',
    aspect: '9:16',
    duration_s: 12,
    caption_style: 'large readable captions',
    cta: 'Watch the full breakdown'
  },
  'meta-feed': {
    label: 'Meta Feed',
    aspect: '1:1',
    duration_s: 8,
    caption_style: 'quiet product captions',
    cta: 'Shop now'
  },
  'meta-story': {
    label: 'Meta Story',
    aspect: '9:16',
    duration_s: 7,
    caption_style: 'minimal story captions',
    cta: 'Swipe'
  },
  'linkedin-feed': {
    label: 'LinkedIn Feed',
    aspect: '4:5',
    duration_s: 15,
    caption_style: 'professional proof captions',
    cta: 'See the details'
  }
};

export function buildCampaignFactoryPlan(values: ProductionStackValues = {}): CampaignFactoryPlan {
  const brand = values.brand || DEFAULT_BRAND;
  const productName = clean(values.productName) || 'approved product';
  const offer = clean(values.offer) || 'the current offer';
  const audience = clean(values.audience) || 'the target buyer';
  const promise = clean(values.promise) || `make ${audience} care quickly`;
  const projectSlug = slugify(values.projectSlug || `${productName}-${offer}`, 'campaign-factory');
  const brandKit = summarizeBrandKit({ ...values, brand, productName, offer });
  const channels = normalizeChannels(values.channels);

  const hooks = buildHooks(productName, offer, audience, promise);
  const scenes = buildScenes(productName, offer, audience, promise, brandKit);
  const variants = channels.map((channel, index) => variantFor(channel, index === 0));

  return {
    schema_version: '1.0.0',
    brand,
    project_slug: projectSlug,
    product_name: productName,
    offer,
    audience,
    promise,
    identity_locks: brandKit.locks,
    required_assets: brandKit.missing,
    hooks,
    scenes,
    variants,
    handoff: {
      recommended_workspace: 'video-studio',
      suggested_model: 'seedance-2-fast',
      image_prompt: `${productName} hero frame for ${audience}. ${promise}. Clean brand-safe composition, no unreadable text.`,
      video_prompt: `${productName} campaign for ${audience}. Open with: ${hooks[0].line}. Show ${offer}. Resolve with a clear product lock and CTA.`,
      tts_script: `${hooks[0].line} ${offer}. ${promise}.`
    }
  };
}

export function buildProductionCanvas(campaign: CampaignFactoryPlan): ProductionCanvas {
  const nodes: CanvasNode[] = [
    node('brief', 'brief', 'Brief', 'ready', 0, 0, `${campaign.product_name} for ${campaign.audience}`),
    node('brand-kit', 'brand_kit', 'Brand Kit', campaign.required_assets.length ? 'needs_input' : 'ready', 220, 0, `${campaign.brand} locks and missing refs`),
    node('identity', 'identity', 'Identity Lock', lockStatus(campaign.identity_locks.product_ref), 440, 0, 'Product, logo, character, and voice state'),
    node('image-refs', 'reference', 'Reference Frames', lockStatus(campaign.identity_locks.product_ref), 660, 0, campaign.handoff.image_prompt),
    node('voice', 'voice', 'Voice Script', campaign.identity_locks.voice.status === 'missing' ? 'planned' : 'ready', 660, 170, campaign.handoff.tts_script),
    node('video-draft', 'video', 'Video Draft', 'planned', 880, 80, campaign.handoff.video_prompt),
    node('post', 'post', 'Post Pass', 'planned', 1100, 80, 'Captions, crop, music, watermark, export naming')
  ];

  campaign.scenes.forEach((scene, index) => {
    nodes.push(node(`scene-${index + 1}`, 'scene', scene.label, scene.required_refs.length ? 'needs_input' : 'planned', 660, 300 + index * 130, scene.prompt));
  });

  campaign.variants.forEach((variant, index) => {
    nodes.push(node(`export-${variant.channel}`, 'export', variant.label, 'planned', 1320, index * 120, `${variant.aspect}, ${variant.duration_s}s, ${variant.caption_style}`));
  });

  const edges: CanvasEdge[] = [
    edge('brief', 'brand-kit', 'locks'),
    edge('brand-kit', 'identity', 'identity'),
    edge('identity', 'image-refs', 'refs'),
    edge('image-refs', 'video-draft', 'frames'),
    edge('voice', 'video-draft', 'audio/script'),
    edge('video-draft', 'post', 'render'),
    ...campaign.scenes.map((scene, index) => edge('image-refs', `scene-${index + 1}`, scene.visual_role)),
    ...campaign.scenes.map((_scene, index) => edge(`scene-${index + 1}`, 'video-draft', 'scene')),
    ...campaign.variants.map((variant) => edge('post', `export-${variant.channel}`, variant.channel))
  ];

  return {
    schema_version: '1.0.0',
    project_slug: campaign.project_slug,
    nodes,
    edges
  };
}

export function buildDirectorShotPlan(values: DirectorShotValues = {}): DirectorShotPlan {
  const subject = clean(values.subject) || 'approved product or character';
  const duration = clampNumber(values.duration, 6, 1, 12);
  const aspect = clean(values.aspect) || '9:16';
  const cameraProfile = clean(values.cameraProfile) || 'controlled-product';
  const lens = clean(values.lens) || '50mm product lens';
  const movement = clean(values.movement) || 'slow push-in';
  const continuityLocks = normalizeList(values.continuityLocks, ['subject silhouette', 'material detail']);
  const audioMode = values.audioMode || 'silent';
  const segments = directorSegments(duration, movement, subject);

  return {
    schema_version: '1.0.0',
    subject,
    duration,
    aspect,
    prompt_addendum: `${cameraProfile} direction, ${lens}, ${movement}. Preserve ${continuityLocks.join(', ')}.`,
    control_stack: [
      { label: 'Profile', value: cameraProfile },
      { label: 'Lens', value: lens },
      { label: 'Movement', value: movement },
      { label: 'Aspect', value: aspect },
      { label: 'Audio', value: audioMode }
    ],
    checklist: [
      'Start frame identity is readable before motion begins',
      'No synthetic text appears unless supplied as a reference',
      'Subject stays inside the central safe area',
      'End frame resolves cleanly for crop and captions'
    ],
    shot_contract_inline: {
      primary_action: `${subject} holds a single coherent hero action while the camera performs ${movement}`,
      subject_focus: subject,
      camera_profile: cameraProfile,
      lens_lock: lens,
      continuity_locks: continuityLocks,
      motion_budget: 'one readable motion idea; avoid chaotic secondary movement',
      lighting_lock: 'stable motivated light with no flicker or exposure pumping',
      audio_lock: { mode: audioMode },
      time_segments: segments,
      negative_prompt: ['warped identity', 'extra objects', 'unreadable text', 'flicker', 'broken product geometry']
    }
  };
}

export function buildPostProductionPlan(
  campaign: CampaignFactoryPlan,
  options: PostProductionOptions = {}
): PostProductionPlan {
  const captions = options.captions !== false;
  const music = Boolean(options.music);
  const watermark = options.watermark !== false;

  return {
    schema_version: '1.0.0',
    project_slug: campaign.project_slug,
    tasks: campaign.variants.map((variant) => {
      const steps = [
        `crop ${variant.aspect}`,
        `trim to ${variant.duration_s}s`
      ];
      if (captions) {
        steps.push('burn captions');
      }
      if (music) {
        steps.push('mix music bed');
      }
      if (watermark) {
        steps.push('apply brand/logo bug');
      }
      steps.push('write metadata sidecar');

      return {
        id: `post-${variant.channel}`,
        channel: variant.channel,
        label: variant.label,
        aspect: variant.aspect,
        duration_s: variant.duration_s,
        output_name: `${campaign.project_slug}-${variant.channel}-${variant.aspect.replace(':', 'x')}.mp4`,
        steps
      };
    })
  };
}

export function buildProductionHandoffRequests(
  campaign: CampaignFactoryPlan,
  director: DirectorShotPlan
): ProductionHandoffRequests {
  const productReference = campaign.identity_locks.product_ref.value;
  const productRefs = productReference ? [productReference] : [];
  const primaryVariant = campaign.variants[0];
  const voiceText = campaign.handoff.tts_script;
  const shotContract = {
    ...director.shot_contract_inline,
    source_frame_read: productReference
      ? 'the product reference image defines product identity, shape, materials, and scale'
      : 'start frame required before video generation; use image handoff to create one first',
    campaign_scenes: campaign.scenes.map((scene) => ({
      id: scene.id,
      label: scene.label,
      visual_role: scene.visual_role,
      duration_s: scene.duration_s,
      prompt: scene.prompt
    })),
    campaign_variants: campaign.variants.map((variant) => ({
      channel: variant.channel,
      aspect: variant.aspect,
      duration_s: variant.duration_s,
      caption_style: variant.caption_style
    }))
  };

  const imageRequest: ImageStudioRequest = {
    brand: campaign.brand,
    project_slug: `${campaign.project_slug}-hero-frame`,
    intent: 'premium',
    mode: productRefs.length ? 'image_to_image' : 'text_to_image',
    aspect: primaryVariant?.aspect || '9:16',
    prompt: campaign.handoff.image_prompt,
    references: productRefs,
    resolution: '2K',
    notes: 'Campaign hero/start frame from Campaign Stack',
    workflow: {
      kind: 'campaign_hero_frame',
      compose_recipe: false,
      product_name: campaign.product_name,
      offer: campaign.offer,
      audience: campaign.audience,
      required_for_video: true
    }
  };

  const videoRequest: VideoStudioPreflightRequest = {
    brand: campaign.brand,
    project_slug: `${campaign.project_slug}-video`,
    model_slug: 'seedance-2-fast',
    prompt: campaign.handoff.video_prompt,
    aspect: primaryVariant?.aspect || director.aspect,
    duration: Math.min(primaryVariant?.duration_s || director.duration, 5),
    resolution: '720p',
    shot_contract_inline: shotContract,
    subject: campaign.product_name,
    action: campaign.promise,
    mood: 'clean campaign motion',
    text_overlay: primaryVariant?.caption_style || 'clean creator captions',
    audio_recipe: director.shot_contract_inline.audio_lock.mode,
    continuity_locks: director.shot_contract_inline.continuity_locks,
    workflow: {
      kind: 'campaign_video',
      source: 'genright_campaign_stack',
      campaign_project_slug: campaign.project_slug,
      scenes: campaign.scenes,
      variants: campaign.variants
    },
    ...(productReference ? { start_frame_path: productReference } : {})
  };

  const voiceRequest: VoiceHandoffRequest = {
    view: 'voice',
    kind: 'voice',
    provider: 'elevenlabs',
    model: 'eleven_v4_turbo',
    prompt: voiceText,
    params: {},
    label: `${campaign.project_slug}-voice`,
    workflow: {
      kind: 'campaign_voice',
      brand: campaign.brand,
      emotion_tags: ['campaign', 'clear', campaign.brand.toLowerCase()],
      campaign_project_slug: campaign.project_slug
    }
  };

  return {
    image: {
      status: productReference ? 'ready' : 'ready_with_warnings',
      reason: productReference ? null : 'No product reference set; image handoff will start from text and may drift.',
      request: imageRequest
    },
    video: {
      status: productReference ? 'ready' : 'blocked',
      reason: productReference ? null : 'Seedance video handoff requires a product reference/start frame.',
      request: videoRequest
    },
    voice: {
      status: voiceText ? 'ready' : 'blocked',
      reason: voiceText ? null : 'Voice handoff requires a generated script.',
      request: voiceRequest
    }
  };
}

export function buildProductionPack({
  campaign,
  canvas,
  director,
  post,
  handoff
}: {
  campaign: CampaignFactoryPlan;
  canvas: ProductionCanvas;
  director: DirectorShotPlan;
  post: PostProductionPlan;
  handoff: ProductionHandoffRequests;
}): ProductionPack {
  return {
    schema_version: '1.0.0',
    kind: 'campaign_production_pack',
    project_slug: campaign.project_slug,
    created_source: 'genright_campaign_stack',
    campaign,
    canvas,
    director,
    post,
    handoff
  };
}

export function summarizeBrandKit(values: ProductionStackValues = {}): BrandKitSummary {
  const brand = values.brand || DEFAULT_BRAND;
  const productName = clean(values.productName) || 'Product';
  const offer = clean(values.offer) || 'Offer';
  const productRef = lock('product_ref', 'Product reference', clean(values.productReferencePath), 'Required for product fidelity');
  const logo = lock('logo', 'Logo / mark', clean(values.logoPath), 'Required for final campaign export');
  const character = optionalLock('character', 'Character / host', clean(values.characterId), 'Optional unless the ad uses a person or recurring host');
  const voice = optionalLock('voice', 'Voice', clean(values.voiceId), 'Optional for silent drafts; ready for VO handoff when set');
  const locks = {
    product_ref: productRef,
    logo,
    character,
    voice
  };
  const missing = [productRef, logo].filter((item) => item.status === 'missing');

  return {
    brand,
    readiness: missing.length ? 'needs_refs' : 'ready',
    locks: {
      ...locks,
      product_ref: {
        ...productRef,
        note: `${productRef.note}: ${productName}`
      },
      logo: {
        ...logo,
        note: `${logo.note}: ${offer}`
      }
    },
    missing
  };
}

function buildHooks(productName: string, offer: string, audience: string, promise: string): CampaignHook[] {
  return [
    {
      id: 'hook-problem',
      label: 'Problem',
      angle: 'pain-open',
      line: `${audience} keep settling for gear that does not fit the job.`
    },
    {
      id: 'hook-product',
      label: 'Product Reveal',
      angle: 'show-the-object',
      line: `${productName} makes the upgrade obvious in the first three seconds.`
    },
    {
      id: 'hook-promise',
      label: 'Promise',
      angle: 'desired-state',
      line: `${promise}.`
    },
    {
      id: 'hook-offer',
      label: 'Offer',
      angle: 'offer-close',
      line: `${offer}, built for ${audience}.`
    }
  ];
}

function buildScenes(
  productName: string,
  offer: string,
  audience: string,
  promise: string,
  brandKit: BrandKitSummary
): CampaignScene[] {
  const needsProductRef = brandKit.locks.product_ref.status === 'missing';
  const productRefs = needsProductRef ? ['product_ref'] : [];
  return [
    {
      id: 'scene-1',
      label: 'Hook Frame',
      duration_s: 2,
      visual_role: 'attention',
      prompt: `Fast clean opener for ${audience}: frame the problem before revealing ${productName}.`,
      voiceover: `${audience} notice this immediately.`,
      required_refs: []
    },
    {
      id: 'scene-2',
      label: 'Product Lock',
      duration_s: 3,
      visual_role: 'identity',
      prompt: `Hero product frame of ${productName}. Preserve shape, materials, logo placement, and scale.`,
      voiceover: `${productName} keeps the promise: ${promise}.`,
      required_refs: productRefs
    },
    {
      id: 'scene-3',
      label: 'Use Moment',
      duration_s: 3,
      visual_role: 'proof',
      prompt: `Show ${productName} in a realistic use moment for ${audience}; no extra text artifacts.`,
      voiceover: `The useful part is visible, not explained.`,
      required_refs: productRefs
    },
    {
      id: 'scene-4',
      label: 'Offer Close',
      duration_s: 2,
      visual_role: 'conversion',
      prompt: `Resolve into a clean final lockup for ${offer}. Keep negative space for captions and CTA.`,
      voiceover: `${offer}.`,
      required_refs: brandKit.locks.logo.status === 'missing' ? ['logo'] : []
    }
  ];
}

function variantFor(channel: CampaignChannel, primary: boolean): CampaignVariant {
  const spec = CHANNEL_SPECS[channel];
  return {
    id: `variant-${channel}`,
    channel,
    label: spec.label,
    aspect: spec.aspect,
    duration_s: spec.duration_s,
    caption_style: spec.caption_style,
    cta: spec.cta,
    priority: primary ? 'primary' : 'draft'
  };
}

function node(
  id: CanvasNode['id'],
  type: CanvasNode['type'],
  label: string,
  status: CanvasNode['status'],
  x: number,
  y: number,
  summary: string
): CanvasNode {
  return { id, type, label, status, x, y, summary };
}

function edge(from: string, to: string, label: string): CanvasEdge {
  return { from, to, label };
}

function lockStatus(lockValue: ProductionLock): CanvasNode['status'] {
  return lockValue.status === 'ready' || lockValue.status === 'optional' ? 'ready' : 'needs_input';
}

function directorSegments(duration: number, movement: string, subject: string) {
  const a = round(duration * 0.3);
  const b = round(duration * 0.7);
  return [
    {
      start_s: 0,
      end_s: a,
      camera: `establish the subject before ${movement}`,
      action: `${subject} is readable and stable`,
      forbidden_zone: 'no immediate morphing, crop loss, or text artifacts'
    },
    {
      start_s: a,
      end_s: b,
      camera: movement,
      action: 'main motion beat',
      forbidden_zone: 'no secondary chaos, duplicate objects, or warped identity'
    },
    {
      start_s: b,
      end_s: duration,
      camera: 'settle into final lock',
      action: 'hold a clean exportable end frame',
      forbidden_zone: 'no late new props, no logo smearing, no exposure pump'
    }
  ];
}

function lock(id: string, label: string, value: string, note: string): ProductionLock {
  return {
    id,
    label,
    status: value ? 'ready' : 'missing',
    value: value || null,
    note
  };
}

function optionalLock(id: string, label: string, value: string, note: string): ProductionLock {
  return {
    id,
    label,
    status: value ? 'ready' : 'optional',
    value: value || null,
    note
  };
}

function normalizeChannels(channels: CampaignChannel[] | undefined): CampaignChannel[] {
  const seen = new Set<CampaignChannel>();
  const normalized = (channels || DEFAULT_CHANNELS)
    .filter((channel): channel is CampaignChannel => Boolean(channel && CHANNEL_SPECS[channel]))
    .filter((channel) => {
      if (seen.has(channel)) {
        return false;
      }
      seen.add(channel);
      return true;
    });
  return normalized.length ? normalized : DEFAULT_CHANNELS;
}

function normalizeList(value: string[] | undefined, fallback: string[]) {
  const normalized = Array.isArray(value)
    ? value.map(clean).filter(Boolean).slice(0, 6)
    : [];
  return normalized.length ? normalized : fallback;
}

function clampNumber(value: number | undefined, fallback: number, min: number, max: number) {
  const number = Number(value);
  if (!Number.isFinite(number)) {
    return fallback;
  }
  return Math.max(min, Math.min(max, Math.round(number)));
}

function round(value: number) {
  return Math.round(value * 10) / 10;
}

function clean(value: string | undefined | null) {
  return typeof value === 'string' ? value.trim() : '';
}

function slugify(value: string | undefined, fallback: string) {
  const slug = clean(value)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 80);
  return slug || fallback;
}
