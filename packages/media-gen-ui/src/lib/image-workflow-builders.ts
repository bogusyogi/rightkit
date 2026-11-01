// Reference only, deleted at QA close.
import type { Brand, ImageStudioRequest } from './types.js';

export interface ImageWorkflowFormValues {
  brand?: Brand;
  projectSlug?: string;
  routeId?: string;
  aspect?: string;
  subject?: string;
  scene?: string;
  style?: string;
  colors?: string;
  negativePrompt?: string;
  references?: string[];
  characterId?: string;
  aestheticLoraId?: string;
  count?: number;
  language?: string;
  outputFolder?: string;
  generationParams?: Record<string, unknown>;
}

export interface ArticleIllustrationPromptPacket {
  workflow_id: 'article-illustrations';
  project_slug: string;
  language: string;
  count: number;
  output_folder?: string;
  shots: Array<{
    index: number;
    title: string;
    anchor: string;
  }>;
  prompts: Array<{
    index: number;
    aspect: '16:9';
    prompt: string;
  }>;
}

const DEFAULT_BRAND: Brand = 'DD';

export function buildImageWorkflowRequest(workflowId: string, values: ImageWorkflowFormValues): ImageStudioRequest {
  if (workflowId === 'character-portrait') {
    return characterPortrait(values);
  }
  if (workflowId === 'nb2-composition') {
    return nb2Composition(values);
  }
  if (workflowId === 'qwen-cartoon-scene') {
    return qwenCartoonScene(values);
  }
  if (workflowId === 'flux-product-photo') {
    return fluxProductPhoto(values);
  }
  if (workflowId === 'recraft-logo-icon') {
    return recraftDesign(values);
  }
  if (workflowId === 'article-illustrations') {
    return articleIllustration(values);
  }
  throw new Error(`Unsupported image workflow "${workflowId}"`);
}

function characterPortrait(values: ImageWorkflowFormValues): ImageStudioRequest {
  const characterId = typeof values.characterId === 'string' ? values.characterId.trim() : '';
  if (!characterId) {
    throw new Error('Character is required for the Character Portrait workflow');
  }
  const subject = [
    values.scene || 'Place the selected character in a polished editorial portrait scene.',
    values.style || 'cinematic editorial portrait, natural pose, coherent wardrobe, flattering light'
  ].join(' ');

  return baseRequest(values, {
    intent: 'mid',
    mode: 'text_to_image',
    aspect: values.aspect || '3:4',
    prompt: subject,
    references: [],
    character_id: characterId,
    ...(values.aestheticLoraId ? { aesthetic_lora_id: values.aestheticLoraId } : {}),
    notes: 'Character Portrait',
    workflow: {
      kind: 'character_portrait',
      compose_recipe: true,
      subject,
      locked_model: 'flux-2-pro',
      lora_policy: 'text-only on flux-2-pro (the served route takes no reference images); a character with a hosted trained LoRA locks onto fal-ai/flux-2/lora (fal-ai/flux-2/lora/edit with reference images) via Character Lock, and a refs-only character needs a reference-capable route the engine lists',
      character_id: characterId,
      ...(values.aestheticLoraId ? { aesthetic_lora_id: values.aestheticLoraId } : {})
    }
  });
}

function nb2Composition(values: ImageWorkflowFormValues): ImageStudioRequest {
  const references = normalizedRefs(values.references);
  const subject = [
    values.subject || 'Create a polished commercial composition.',
    values.scene || 'Use the selected references as the source of truth.',
    references.length ? 'Use @image1 and the other provided references explicitly; preserve identity, product shape, text, and layout-critical details.' : ''
  ].filter(Boolean).join(' ');
  return baseRequest(values, {
    intent: 'premium',
    mode: references.length ? 'image_to_image' : 'text_to_image',
    aspect: values.aspect || '1:1',
    prompt: subject,
    references,
    resolution: '2K',
    notes: 'NB2 Multi-Reference Composition',
    workflow: {
      kind: 'nb2_composition',
      compose_recipe: true,
      subject,
      locked_model: 'NB2',
      locked_resolution: '2K'
    }
  });
}

function qwenCartoonScene(values: ImageWorkflowFormValues): ImageStudioRequest {
  const subject = [
    values.subject || 'A charming illustrated character',
    values.scene || 'in a clear storybook scene',
    values.style || 'soft painterly cartoon style'
  ].join(', ');
  return baseRequest(values, {
    intent: 'cartoon',
    mode: 'text_to_image',
    aspect: values.aspect || '1:1',
    prompt: subject,
    references: [],
    notes: 'Qwen Cartoon Scene',
    workflow: {
      kind: 'qwen_cartoon_scene',
      compose_recipe: true,
      subject,
      locked_model: 'qwen-image'
    }
  });
}

// The served flux-2-pro route takes a text prompt only, so Product Photo is text-to-image.
// Supplied references are refused rather than dropped, matching the unavailable Recraft vector route.
function fluxProductPhoto(values: ImageWorkflowFormValues): ImageStudioRequest {
  if (normalizedRefs(values.references).length > 0) {
    throw new Error('Reference images are unavailable for Product Photo: the served FLUX.2 Pro route takes a text prompt only');
  }
  const subject = [
    values.subject || 'A premium product hero still',
    values.scene || 'in a clean editorial setting',
    values.style || 'photoreal, natural light, crisp material detail'
  ].filter(Boolean).join(' ');
  return baseRequest(values, {
    intent: 'mid',
    mode: 'text_to_image',
    aspect: values.aspect || '16:9',
    prompt: subject,
    references: [],
    notes: 'FLUX Product Photo',
    workflow: {
      kind: 'flux_product_photo',
      compose_recipe: true,
      subject,
      has_humans: false,
      lens: '100mm_macro_extreme_close',
      lighting: 'motivated_key',
      locked_model: 'flux-2-pro',
      max_references: 0
    }
  });
}

function recraftDesign(values: ImageWorkflowFormValues): ImageStudioRequest {
  const route = recraftRoute(values.routeId);
  if (!route.available) {
    throw new Error(`${route.label} is unavailable: ${route.unavailableReason ?? 'no served route'}`);
  }
  const subject = [
    values.subject || 'A clean brand-ready icon or design asset',
    values.scene || 'centered composition with simple geometry',
    route.promptHint,
    values.colors ? `Color direction: ${values.colors}.` : ''
  ].filter(Boolean).join(' ');

  return baseRequest(values, {
    intent: route.intent,
    mode: 'text_to_image',
    aspect: values.aspect || '1:1',
    prompt: subject,
    references: [],
    notes: route.label,
    workflow: {
      kind: 'recraft_design',
      compose_recipe: true,
      subject,
      has_humans: false,
      route_id: route.id,
      locked_model: route.model
    }
  });
}

export function buildArticleIllustrationPromptPacket(values: ImageWorkflowFormValues): ArticleIllustrationPromptPacket {
  const count = clampCount(values.count, 4);
  const language = values.language || (/[\u3400-\u9fff]/.test(values.subject || '') ? 'zh' : 'en');
  const anchors = articleAnchors(values.subject || values.scene || 'A single clear article concept.', count);
  const shots = Array.from({ length: count }, (_item, index) => {
    const anchor = anchors[index] || anchors[anchors.length - 1] || 'A single clear article concept.';
    return {
      index: index + 1,
      title: `Anchor ${index + 1}`,
      anchor
    };
  });

  return {
    workflow_id: 'article-illustrations',
    project_slug: slugify(values.projectSlug, 'article-illustrations'),
    language,
    count,
    ...(values.outputFolder ? { output_folder: values.outputFolder } : {}),
    shots,
    prompts: shots.map((shot) => ({
      index: shot.index,
      aspect: '16:9',
      prompt: articlePrompt(shot.anchor, language)
    }))
  };
}

function articleIllustration(values: ImageWorkflowFormValues): ImageStudioRequest {
  const packet = buildArticleIllustrationPromptPacket(values);
  const first = packet.prompts[0];
  return baseRequest(values, {
    intent: 'text_premium',
    mode: 'text_to_image',
    aspect: '16:9',
    prompt: first.prompt,
    references: [],
    quality: 'medium',
    notes: 'Article Illustrations',
    workflow: {
      kind: 'article_illustrations',
      prompt_packet: true,
      language: packet.language,
      count: packet.count,
      shot_list: packet.shots,
      prompts: packet.prompts,
      output_folder: packet.output_folder || null
    }
  });
}

function baseRequest(
  values: ImageWorkflowFormValues,
  request: Omit<ImageStudioRequest, 'brand' | 'project_slug'>
): ImageStudioRequest {
  const next: ImageStudioRequest = {
    ...request,
    brand: values.brand || DEFAULT_BRAND,
    project_slug: slugify(values.projectSlug, 'workflow-image')
  };
  if (values.generationParams && Object.keys(values.generationParams).length > 0) {
    next.generation_params = { ...values.generationParams };
  }
  return next;
}

interface RecraftRoute {
  id: string;
  label: string;
  intent: string;
  model: string;
  promptHint: string;
  available: boolean;
  unavailableReason?: string;
}

// The served WaveSpeed Recraft V4 route is text-to-image (raster). No served route produces SVG,
// so the vector route is listed as unavailable and is never the default.
function recraftRoute(routeId = 'recraft-raster'): RecraftRoute {
  if (routeId === 'imagen-safe') {
    return {
      id: 'imagen-safe',
      label: 'Imagen Safe Fallback',
      intent: 'google_safe',
      model: 'imagen-4-ultra',
      promptHint: 'safe clean commercial render, simple and non-controversial.',
      available: true
    };
  }
  if (routeId === 'recraft-vector') {
    return {
      id: 'recraft-vector',
      label: 'Recraft Vector (unavailable)',
      intent: 'vector',
      model: 'recraft-v4-vector',
      promptHint: 'vector illustration, clean edges, icon-ready, simple background.',
      available: false,
      unavailableReason: 'no served route produces SVG'
    };
  }
  return {
    id: 'recraft-raster',
    label: 'Recraft Layout',
    intent: 'design_layout',
    model: 'recraft-v4-raster',
    promptHint: 'digital illustration layout, polished flat design, brand-safe.',
    available: true
  };
}

function normalizedRefs(references: string[] | undefined) {
  return Array.isArray(references) ? references.filter((item) => typeof item === 'string' && item.trim()) : [];
}

function articleAnchors(text: string, count: number) {
  const anchors = text
    .split(/\r?\n|[。.!?？；;]/)
    .map((item) => item.trim())
    .filter(Boolean);
  if (anchors.length >= count) {
    return anchors.slice(0, count);
  }
  return anchors.length ? anchors : [text.trim()].filter(Boolean);
}

function articlePrompt(anchor: string, language: string) {
  const annotation = language.toLowerCase().startsWith('zh')
    ? 'sparse red, orange, or blue handwritten Chinese annotations'
    : 'sparse red, orange, or blue handwritten annotations';
  return [
    `Explain this article idea in one image: ${anchor}.`,
    '16:9 PNG, pure white background.',
    'Minimal black hand-drawn line art with generous empty space.',
    'Optional small solid-black hand-drawn figure with white dot eyes and thin legs, performing the core conceptual action.',
    `${annotation}, only a few short labels.`,
    'One image explains one core idea; avoid slide styling, formal diagrams, cute mascot poster energy, dense explainers, shadows, and paper texture.'
  ].join(' ');
}

function clampCount(value: number | undefined, fallback: number) {
  const number = Number(value);
  if (!Number.isFinite(number)) {
    return fallback;
  }
  return Math.max(1, Math.min(8, Math.round(number)));
}

function slugify(value: string | undefined, fallback: string) {
  const slug = String(value || '')
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60);
  return slug || fallback;
}
