// Reference only, deleted at QA close.
import type { Brand, VideoStudioPreflightRequest } from './types';

export type MotionFlowType = 'classic_motion' | 'kinetic_motion';

export interface MotionGraphicsValues {
  brand?: Brand;
  projectSlug?: string;
  flowType?: MotionFlowType;
  aspect?: string;
  duration?: number;
  subject?: string;
  mood?: string;
  modelSlug?: string;
}

export function buildMotionGraphicsVideoRequest(
  values: MotionGraphicsValues = {}
): VideoStudioPreflightRequest & { workflow: Record<string, unknown> } {
  const duration = clampDuration(values.duration, 4);
  const flowType = values.flowType || 'classic_motion';
  const subject = clean(values.subject) || 'the approved product or brand asset';
  const mood = clean(values.mood) || 'clean premium launch motion';
  const modelSlug = values.modelSlug || 'seedance-2-fast';

  return {
    brand: values.brand || 'DD',
    project_slug: slugify(values.projectSlug, 'motion-graphics'),
    model_slug: modelSlug,
    prompt: [
      `${flowType === 'kinetic_motion' ? 'Kinetic motion' : 'Classic motion'} promo for ${subject}.`,
      mood,
      flowType === 'kinetic_motion'
        ? 'Fast camera energy, match-cut feeling, clear start and end lock; do not rely on readable AI text.'
        : 'Smooth restrained camera, clean transitions, clear start and end logo lock.'
    ].join(' '),
    aspect: values.aspect || '9:16',
    duration,
    resolution: modelSlug.startsWith('seedance') ? '480p' : undefined,
    shot_contract_inline: motionShotContract(subject, duration, flowType),
    workflow: {
      kind: 'motion_graphics',
      flow_type: flowType,
      storyboard: [
        'open on approved start frame',
        flowType === 'kinetic_motion' ? 'quick motivated motion beat' : 'smooth hero motion beat',
        'resolve into clean end-frame lock'
      ],
      suggested_start_frame_prompt: `${subject}, clean white or brand-color setup, no extra text`,
      suggested_end_frame_prompt: `${subject}, final hero lockup with clean negative space`
    }
  };
}

function motionShotContract(subject: string, duration: number, flowType: MotionFlowType) {
  return {
    primary_action: flowType === 'kinetic_motion'
      ? `${subject} moves through one energetic but controlled transition and resolves cleanly`
      : `${subject} moves through one smooth controlled transition and resolves cleanly`,
    subject_focus: subject,
    source_frame_read: 'the approved start frame defines subject identity, composition, and material details',
    role_blocking: {
      hands_left: 'absent unless present in the approved reference frame',
      hands_right: 'absent unless present in the approved reference frame',
      gaze: 'absent unless a human subject is present in the approved reference frame',
      body_orientation: 'unchanged unless a human subject is present in the approved reference frame'
    },
    device_role: 'absent',
    allowed_contact_surfaces: ['approved reference-frame surfaces'],
    motion_budget: 'subject remains recognizable and inside the central safe area throughout the clip',
    time_segments: [{
      start_s: 0,
      end_s: duration,
      camera: flowType === 'kinetic_motion'
        ? 'Faster but controlled camera movement; no chaotic shake, no unreadable typography.'
        : 'Restrained smooth camera movement; no chaotic shake, no unreadable typography.',
      action: 'one coherent motion beat from start frame to end frame',
      forbidden_zone: 'no extra objects, warped product geometry, or synthetic text artifacts enter the frame'
    }],
    lighting_lock: 'stable soft light with no flicker or exposure pumping',
    audio_lock: { mode: 'silent' },
    negative_prompt: ['warped subject', 'extra objects', 'unreadable text artifacts']
  };
}

function clampDuration(value: number | undefined, fallback: number) {
  const number = Number(value);
  if (!Number.isFinite(number)) {
    return fallback;
  }
  return Math.max(1, Math.min(12, Math.round(number)));
}

function clean(value: string | undefined) {
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
