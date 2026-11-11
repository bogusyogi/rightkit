import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { StudioPanels } from './panels/StudioPanels.js';
import { assetUrl, errorCode, errorMessage, estimateGeneration, fetchModelPriceFacts, installLocalImageProfile, isTauri, type GenerateRequest, type MediaKind, type ModelEntry, type ModelPriceFacts, saveReferencePng } from '../lib/api.js';
import { ASSUMED_ROUNDING_SENTENCE, UNCONFIRMED_PRICE_LABEL, UNCONFIRMED_PRICE_SENTENCE, UPSCALE_SCALES, addReference, avatarCaps, buildMediaParams, buildParams, durationChoices, durationSpec, imageCaps, isAssumedRounding, isKeyless, isUnconfirmedPrice, isValidDuration, lipsyncCaps, musicCaps, preferredProvider, priceFactNotes, reconcileDuration, referenceCaps, sfxCaps, upscaleCaps, videoCaps, voiceIdSpec, withReferenceParams, type ReferenceCaps } from '../lib/gen-params.js';
import { noModelMessage, pickOtherProviderModel, priceCaption } from '../lib/model-select.js';
import { handoffFormValues, onHandoff, takeHandoff, type Handoff, type HandoffView } from '../lib/handoff.js';
import { jobIsActive, useApp, type SessionJob } from '../lib/app-state.js';
import type { SettingsSectionId, ViewId } from '../lib/nav.js';
import { JobMeta, JobOutputs } from '../components/JobCard.js';
import { IconPlay } from '../components/icons.js';
import { MaskEditor } from '../components/MaskEditor.js';
import { maskInputOf, startInputOf, withMaskParam, withStartParam } from '../lib/mask-params.js';
import { Card, Badge, Button, Checkbox, EmptyState, IconButton, Input, Notice, Popover, ProgressBar, SegmentedControl, Select } from '@rightkit/react-shell/react';
import { useListNavigation } from '@rightkit/platform-ui/headless';
import { Kv, baseName, formatBytes, providerName, usd } from '../components/ui.js';
import { MediaPlayer } from '@rightkit/media-ui';
import { ErrorNote } from '../components/ErrorNote.js';
import { falUploadCount, falUploadNames, needsFalUploadConfirmation, useFalUploadConfirm } from '../components/FalUploadConfirm.js';
import type { LocalImageInstallProgress, LocalImageProfile } from '../lib/types.js';
import { LocalImageChat, type LocalChatHandle } from './local/LocalImageChat.js';
import { LocalPanel } from './local/LocalPanel.js';
import { installView } from './local/logic.js';
import { useLocalImageLive } from './local/useLocalImageLive.js';
import { RoutePanel } from './RoutePanel.js';

interface Option {
  id: string;
  source: 'cloud' | 'local';
  provider: string;
  model: string;
  name: string;
  sub: string;
  est: number;
  profile?: LocalImageProfile;
  entry?: ModelEntry;
}

const UNIT: Record<MediaKind, string> = { image: 'image', video: 'clip', voice: 'request', avatar: 'video', music: 'track', sfx: 'sound', upscale: 'file', lipsync: 'video' };
const LOCAL_ASPECTS = ['1:1', '16:9', '9:16', '4:3'];
const LOCAL_SIZE: Record<string, [number, number]> = { '1:1': [1024, 1024], '16:9': [1344, 768], '9:16': [768, 1344], '4:3': [1152, 864] };

const COPY: Record<MediaKind, { title: string; eyebrow: string; picker: string; placeholder: string; cta: string }> = {
  image: { title: 'Image', eyebrow: 'Create', picker: 'Provider', placeholder: 'Describe the image. Enter to generate, Shift+Enter for a new line.', cta: 'Generate' },
  video: { title: 'Video', eyebrow: 'Create', picker: 'Model', placeholder: 'Describe the motion and the scene.', cta: 'Generate' },
  voice: { title: 'Voice', eyebrow: 'Create', picker: 'Model', placeholder: 'Type the script to speak.', cta: 'Generate speech' },
  avatar: { title: 'Video', eyebrow: 'Create', picker: 'Avatar model', placeholder: 'Script the avatar speaks. Leave empty if you give an audio URL (HeyGen).', cta: 'Generate avatar video' },
  music: { title: 'Music', eyebrow: 'Create', picker: 'Model', placeholder: 'Describe the track — genre, mood, instruments, tempo.', cta: 'Generate music' },
  sfx: { title: 'Sound FX', eyebrow: 'Create', picker: 'Model', placeholder: 'Describe the sound, or paste a video URL below for video-to-sound.', cta: 'Generate sound' },
  upscale: { title: 'Upscale', eyebrow: 'Create', picker: 'Model', placeholder: 'Optional note. The input file below drives the job.', cta: 'Upscale' },
  lipsync: { title: 'Lipsync', eyebrow: 'Create', picker: 'Model', placeholder: 'Optional note. The video and audio URLs below drive the job.', cta: 'Sync lips' }
};

const WORKFLOW_TEMPLATES: Record<MediaKind, Array<[string, string]>> = {
  image: [['Product on seamless', 'Hero shot, soft shadow'], ['Book cover draft', '6×9 with title safe area'], ['Social square set', '4 variations, brand palette'], ['Character sheet', 'Front / side / back']],
  video: [['Image to clip', 'Animate a Gallery image'], ['First → last frame', 'Morph between two stills'], ['Product orbit', '360° turn from one shot'], ['Storyboard', 'Shot list to clips']],
  voice: [],
  avatar: [],
  music: [],
  sfx: [],
  upscale: [],
  lipsync: []
};

type Mode = 'chat' | 'form' | 'workflows' | 'avatar';

function modeOptions(kind: MediaKind): Array<{ value: Mode; label: string }> {
  if (kind === 'image') return [{ value: 'chat', label: 'Chat' }, { value: 'form', label: 'Advanced' }];
  if (kind === 'video') return [{ value: 'form', label: 'Generate' }, { value: 'avatar', label: 'Avatar' }];
  return [{ value: 'form', label: 'Generate' }];
}

const EXAMPLES: Record<MediaKind, string[]> = {
  image: ['A quiet forest cabin at dawn, soft fog', 'Product hero on a seamless warm backdrop', 'Isometric cutaway of a tiny bookshop'],
  video: ['Slow orbit around a ceramic mug', 'Cloud timelapse over a city skyline', 'A paper boat drifting down a creek'],
  voice: ['“Chapter one. The house was quiet…”', 'A warm, unhurried product explainer', 'Short voicemail greeting, friendly tone'],
  avatar: ['Welcome back — here is what shipped this week.', 'Introduce the new collection in one line.'],
  music: ['Lo-fi beat, 90 bpm, vinyl crackle', 'Cinematic strings, slow build', 'Chiptune loop for a menu screen'],
  sfx: ['Rain on a tin roof', 'Soft UI click, very short', 'Distant thunder rumble'],
  upscale: [],
  lipsync: []
};

const EMPTY_HINT: Record<MediaKind, string> = {
  image: 'Describe an image and press Enter. Pick a model in the header.',
  video: 'Describe the motion and scene, then press Enter.',
  voice: 'Type a script and press Enter to generate speech.',
  avatar: 'Write the script the avatar speaks, then press Enter.',
  music: 'Describe the track and press Enter.',
  sfx: 'Describe the sound and press Enter.',
  upscale: 'Paste an image path or URL below and press Upscale.',
  lipsync: 'Paste a video and an audio URL below, then press Sync lips.'
};

/** Strip catalog parentheticals ("(image, ~$0.006/MP)") for display; full label stays on tooltips. */
const displayName = (n: string) => n.replace(/\s*\([^()]*\)\s*$/, '').trim() || n;

/** The voice fallback offers another route than the ElevenLabs voice that refused the job (voice_unavailable). */
const VOICE_FALLBACK_AWAY_FROM = 'elevenlabs';

const URL_RE = /^https?:\/\/\S+$/i;

export function Generate({ kind: viewKind, onNavigate }: { kind: MediaKind; onNavigate: (view: ViewId, section?: SettingsSectionId) => void }) {
  const app = useApp();
  const falUpload = useFalUploadConfirm();
  const [mode, setMode] = useState<Mode>(viewKind === 'image' ? 'chat' : 'form');
  // Avatar videos live inside the Video workspace as their own mode.
  const kind: MediaKind = viewKind === 'video' && mode === 'avatar' ? 'avatar' : viewKind;
  const copy = COPY[kind];
  const [prompt, setPrompt] = useState('');
  const [aspect, setAspect] = useState('');
  const [seed, setSeed] = useState('');
  const [resolution, setResolution] = useState('');
  const [duration, setDuration] = useState<number | null>(null);
  const [imageUrl, setImageUrl] = useState('');
  const [endImageUrl, setEndImageUrl] = useState('');
  const [voiceId, setVoiceId] = useState('');
  const [avatarId, setAvatarId] = useState('');
  const [audioUrl, setAudioUrl] = useState('');
  const [videoUrl, setVideoUrl] = useState('');
  const [secs, setSecs] = useState('');
  const [scale, setScale] = useState('4');
  const [syncMode, setSyncMode] = useState('');
  const [loop, setLoop] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<{ code: string | null; message: string } | null>(null);
  const [busy, setBusy] = useState(false);
  // Params and gating contributed by studio panels (see views/panels).
  const [studioParams, setStudioParams] = useState<Record<string, Record<string, unknown>>>({});
  const [studioBlock, setStudioBlock] = useState<Record<string, string | null>>({});
  const msgsRef = useRef<HTMLDivElement | null>(null);
  const localChat = useRef<LocalChatHandle | null>(null);
  const promptRef = useRef<HTMLTextAreaElement | null>(null);
  const [railOpen, setRailOpen] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // Inline composer action registered by a blocking studio panel (e.g. Run preflight).
  const [composerAction, setComposerAction] = useState<{ label: string; run: () => void; disabled?: boolean } | null>(null);
  // Input paths per submitted job so input-first threads can show file names.
  const [jobInputs, setJobInputs] = useState<Record<string, string[]>>({});
  const [inDims, setInDims] = useState<{ w: number; h: number } | null>(null);
  // Reference images for models that take them (image kind only). Sent as params.image_urls.
  const [references, setReferences] = useState<string[]>([]);
  // Saved mask PNG path for models whose catalogue row declares a `mask` input (sent as that input's param).
  const [maskPath, setMaskPath] = useState<string | null>(null);
  // What a hand-off prefilled, shown until the next submit.
  const [handoffNote, setHandoffNote] = useState<string | null>(null);
  // "Why this route" panel in the composer: collapsed until opened; reads routes, never submits.
  const [routeOpen, setRouteOpen] = useState(false);
  // Price facts from media modalities (reconciled, assumed rounding). The models rows do not carry them.
  const [priceFacts, setPriceFacts] = useState<Record<string, ModelPriceFacts>>({});
  useEffect(() => {
    let live = true;
    fetchModelPriceFacts()
      .then((facts) => {
        if (live) setPriceFacts(facts);
      })
      .catch(() => {
        if (live) setPriceFacts({});
      });
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    setMode(viewKind === 'image' ? 'chat' : 'form');
    setAspect('');
    setSelected(null);
    setError(null);
    setPrompt('');
    setVideoUrl('');
    setEndImageUrl('');
    setSecs('');
    setSyncMode('');
    setReferences([]);
    setMaskPath(null);
    setHandoffNote(null);
  }, [viewKind]);

  useEffect(() => {
    setSelected(null);
    setError(null);
  }, [mode]);

  const options = useMemo<Option[]>(() => {
    const cloud: Option[] = app.models
      .filter((m) => m.kind === kind)
      .map((m) => ({ id: `${m.provider}/${m.model}`, source: 'cloud', provider: m.provider, model: m.model, name: m.label, sub: providerName(m.provider), est: m.est_usd, entry: m }));
    const local: Option[] = kind === 'image'
      ? app.localProfiles.map((p) => ({ id: `local:${p.id}`, source: 'local' as const, provider: 'local', model: p.id, name: p.label, sub: p.install_state === 'installed' ? 'ready' : p.install_state === 'installing' ? 'installing…' : 'not installed', est: 0, profile: p }))
      : [];
    const pref = preferredProvider(kind, app.models);
    const all = [...local, ...cloud];
    // Engine order is default-first within a provider; put the preferred provider's group first.
    return pref ? [...all.filter((o) => o.provider === pref), ...all.filter((o) => o.provider !== pref)] : all;
  }, [app.models, app.localProfiles, kind]);

  const preferred = preferredProvider(kind, app.models);
  const current = options.find((o) => o.id === selected)
    ?? options.find((o) => o.entry?.default === true)
    ?? (preferred ? options.find((o) => o.provider === preferred) : undefined)
    ?? options.find((o) => o.source === 'local' && o.profile?.install_state === 'installed')
    ?? options.find((o) => o.source === 'cloud' && app.configured.has(o.provider))
    ?? options[0];

  // Local (sd.cpp) image chat: conversations, queue, install progress. See views/local.
  const localImageActive = kind === 'image' && current?.source === 'local';
  const localLive = useLocalImageLive(localImageActive, () => void app.refreshLocalProfiles());
  const localChatMode = localImageActive && mode === 'chat' && Boolean(current?.profile);

  const entry = current?.source === 'cloud' ? current.entry : undefined;
  // Reference images: shown only for image models that take them (see referenceCaps).
  const refCaps: ReferenceCaps | null = entry ? referenceCaps(entry) : null;
  const maskInput = kind === 'image' ? maskInputOf(entry) : null;
  // A row with a start input and no reference list takes one source image, sent as that param (a single string).
  const startInput = kind === 'image' && !refCaps ? startInputOf(entry) : null;
  const refMax = refCaps ? refCaps.max : startInput ? 1 : 0;
  const dSpec = entry && kind === 'video' ? durationSpec(entry) : null;
  const effDuration = reconcileDuration(dSpec, duration);
  const vCaps = entry && kind === 'video' ? videoCaps(entry) : null;
  const iCaps = entry && kind === 'image' ? imageCaps(entry) : null;
  const aCaps = entry && kind === 'avatar' ? avatarCaps(entry) : null;
  const mCaps = entry && kind === 'music' ? musicCaps(entry) : null;
  const sCaps = entry && kind === 'sfx' ? sfxCaps(entry) : null;
  const uCaps = entry && kind === 'upscale' ? upscaleCaps(entry) : null;
  const lCaps = entry && kind === 'lipsync' ? lipsyncCaps(entry) : null;
  // Voice id box: shown unless the engine says the model reads none.
  const voiceIdOk = kind !== 'voice' || !entry || voiceIdSpec(entry) !== null;
  const aspects = current?.source === 'local' ? LOCAL_ASPECTS : vCaps?.aspects ?? iCaps?.aspects ?? [];
  const effAspect = aspects.includes(aspect) ? aspect : aspects[0] ?? '';
  const resChoices = vCaps?.resolutions ?? iCaps?.resolutions ?? aCaps?.resolutions ?? [];
  const effResolution = resChoices.includes(resolution) ? resolution : (vCaps?.defaultResolution ?? iCaps?.defaultResolution ?? aCaps?.defaultResolution ?? aCaps?.resolutions[0] ?? '');
  const resolutionOptions = [...(effResolution === '' ? [{ value: '', label: 'Model default' }] : []), ...resChoices.map((r) => ({ value: r, label: r }))];
  const durationOk = !dSpec || isValidDuration(dSpec, effDuration);
  // The request exactly as submit would send it. Its estimate comes from the engine (dry run), so audio,
  // sound and resolution surcharges are priced by the same code that reserves spend.
  const estimateRequest = useMemo<GenerateRequest | null>(() => {
    if (!current || current.source !== 'cloud' || !current.entry) return null;
    const text = prompt.trim();
    const mediaKind = kind === 'music' || kind === 'sfx' || kind === 'upscale' || kind === 'lipsync';
    const built = mediaKind
      ? buildMediaParams(current.entry, { prompt: text, secs, imageUrl, videoUrl, audioUrl, scale, syncMode, loop })
      : buildParams(current.entry as ModelEntry, { aspect: effAspect, resolution: effResolution, duration: effDuration, seed, imageUrl, endImageUrl, voiceId, avatarId, audioUrl, prompt: text });
    if (!built.ok) return null;
    const params = { ...withMaskParam(withStartParam(withReferenceParams(built.params, references, refCaps), startInput, references, Boolean(refCaps)), maskInput, maskPath), ...Object.assign({}, ...Object.values(studioParams)) };
    return { kind, provider: current.provider, model: current.model, prompt: text || kind, params };
  }, [current, kind, prompt, secs, imageUrl, videoUrl, audioUrl, scale, syncMode, loop, effAspect, effResolution, effDuration, seed, endImageUrl, voiceId, avatarId, studioParams, references, refCaps, startInput, maskInput, maskPath]);
  const estimateKey = estimateRequest ? JSON.stringify(estimateRequest) : '';
  const [engineEstimate, setEngineEstimate] = useState<{ key: string; usd: number } | null>(null);
  useEffect(() => {
    if (!estimateKey) return;
    let live = true;
    const timer = window.setTimeout(() => {
      estimateGeneration(JSON.parse(estimateKey) as GenerateRequest)
        .then((r) => {
          if (live && typeof r.est_usd === 'number') setEngineEstimate({ key: estimateKey, usd: r.est_usd });
        })
        .catch(() => {
          if (live) setEngineEstimate(null);
        });
    }, 350);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [estimateKey]);
  const engineUsd = estimateKey && engineEstimate?.key === estimateKey ? engineEstimate.usd : null;
  // The engine's dry-run figure for these exact params is the total shown. Until it arrives, or if it fails,
  // the model's default-size estimate is shown with the duration or unit it was made for. The engine marks no
  // starting price, so none is labelled "from".
  const cloudPrice = current && current.source === 'cloud'
    ? (engineUsd != null ? usd(engineUsd) : current.entry ? priceCaption(current.entry, UNIT[kind]) : 'price unknown')
    : '';
  const groups = useMemo(() => {
    const out: Array<{ key: string; title: string; items: Option[] }> = [];
    for (const o of options) {
      const key = o.source === 'local' ? 'local' : o.provider;
      let g = out.find((x) => x.key === key);
      if (!g) {
        g = { key, title: o.source === 'local' ? 'On this computer' : providerName(o.provider), items: [] };
        out.push(g);
      }
      g.items.push(o);
    }
    return out;
  }, [options]);

  // Hand-off from another view (Production). Taken once on arrival and cleared from the store, so a
  // second mount or re-render never applies it again. Prefills the composer only; the person presses Generate.
  const handoffView: HandoffView | null = viewKind === 'image' || viewKind === 'video' || viewKind === 'voice' ? viewKind : null;
  useEffect(() => {
    if (!handoffView) return undefined;
    const apply = (h: Handoff) => {
      const v = handoffFormValues(h);
      const notes = [`Prefilled from ${h.source ?? 'another view'}. Review it, then press ${COPY[viewKind].cta}.`];
      setPrompt(v.prompt);
      setAspect(v.aspect);
      setResolution(v.resolution);
      setDuration(v.duration);
      setSeed(v.seed);
      setImageUrl(v.imageUrl);
      setAudioUrl(v.audioUrl);
      setVideoUrl(v.videoUrl);
      setVoiceId(v.voiceId);
      setError(null);
      const target = v.model ? options.find((o) => o.id === `${v.model?.provider}/${v.model?.model}`) : undefined;
      if (v.model && target) setSelected(target.id);
      else if (v.model) notes.push(`${v.model.model} is not in this view's model list; the current model stays selected.`);
      const caps = target?.entry ? referenceCaps(target.entry) : null;
      if (target) {
        setReferences(caps ? v.references.slice(0, caps.max) : []);
        if (v.references.length && !caps) notes.push('Reference images were not attached: this model takes none.');
        if (caps && v.references.length > caps.max) notes.push(`Only the first ${caps.max} reference images were attached.`);
      } else {
        setReferences(v.references);
      }
      if (v.unapplied.length) notes.push(`Not applied: ${v.unapplied.join(', ')}.`);
      setHandoffNote(notes.join(' '));
    };
    const take = () => {
      const h = takeHandoff(handoffView);
      if (h) apply(h);
    };
    take();
    return onHandoff((h) => {
      if (h.view === handoffView) take();
    });
  }, [handoffView, options]);

  // Poll install progress while a local profile is installing.
  const { refreshLocalProfiles } = app;
  const anyInstalling = app.localProfiles.some((p) => p.install_state === 'installing');
  useEffect(() => {
    if (!anyInstalling) return;
    const t = window.setInterval(() => void refreshLocalProfiles(), 2000);
    return () => window.clearInterval(t);
  }, [anyInstalling, refreshLocalProfiles]);

  const kindJobs = useMemo(() => app.jobs.filter((j) => j.kind === kind), [app.jobs, kind]);
  const thread = useMemo(() => [...kindJobs].reverse(), [kindJobs]);
  const activeCount = kindJobs.filter(jobIsActive).length;

  useEffect(() => {
    const el = msgsRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [thread.length, mode]);

  const keyReady = current ? current.source === 'local' || app.configured.has(current.provider) : false;
  const localReady = current?.source === 'local' && current.profile?.install_state === 'installed';
  const needsInstall = current?.source === 'local' && !localReady;
  const installing = current?.profile?.install_state === 'installing';

  async function run(overridePrompt?: string) {
    const text = (overridePrompt ?? prompt).trim();
    if (!current) return;
    setError(null);
    setHandoffNote(null);
    if (needsInstall && current.profile) {
      try {
        await installLocalImageProfile(current.profile.id);
        await app.refreshLocalProfiles();
      } catch (e) {
        setError({ code: errorCode(e), message: errorMessage(e) });
      }
      return;
    }
    if (localChatMode) {
      localChat.current?.submit();
      return;
    }
    if (!keyReady) {
      onNavigate('settings');
      return;
    }
    const mediaKind = kind === 'music' || kind === 'sfx' || kind === 'upscale' || kind === 'lipsync';
    // The engine requires a non-empty prompt on every request; upscale,
    // lipsync and video-to-sfx are driven by their input URLs, not the text.
    if (!text && !mediaKind && !(kind === 'avatar' && audioUrl.trim())) {
      setError({ code: null, message: kind === 'voice' ? 'Type the script to speak first.' : 'Describe what to generate first.' });
      return;
    }
    setBusy(true);
    try {
      if (current.source === 'local' && current.profile) {
        const [w, h] = LOCAL_SIZE[effAspect] ?? [current.profile.defaults.width, current.profile.defaults.height];
        await app.submitLocal(current.profile, text, w, h);
      } else {
        const built = mediaKind && current.entry
          ? buildMediaParams(current.entry, { prompt: text, secs, imageUrl, videoUrl, audioUrl, scale, syncMode, loop })
          : buildParams(current.entry as ModelEntry, { aspect: effAspect, resolution: effResolution, duration: effDuration, seed, imageUrl, endImageUrl, voiceId, avatarId, audioUrl, prompt: text });
        if (!built.ok) {
          setError({ code: null, message: built.error });
          return;
        }
        const params = { ...withMaskParam(withStartParam(withReferenceParams(built.params, references, refCaps), startInput, references, Boolean(refCaps)), maskInput, maskPath), ...Object.assign({}, ...Object.values(studioParams)) };
        const request: GenerateRequest = { kind, provider: current.provider, model: current.model, prompt: text || kind, params };
        let job: SessionJob;
        try {
          job = await app.submitCloud(request, displayName(current.name), effAspect || undefined);
        } catch (e) {
          // A local reference image goes to the person's own fal storage so NanoBanana can read it;
          // the engine refuses until this run is approved. The approval covers this one submit.
          if (!needsFalUploadConfirmation(e)) throw e;
          const startRef = startInput ? (params as Record<string, unknown>)[startInput.param] : undefined;
          const maskRef = maskInput ? (params as Record<string, unknown>)[maskInput.param] : undefined;
          const refs = Array.isArray(params.image_urls) ? (params.image_urls as unknown[]) : [startRef, maskRef];
          const names = refs.filter((r): r is string => typeof r === 'string' && !/^https?:\/\//i.test(r)).map((r) => baseName(r, 'image'));
          // The engine's details.paths and details.count name what would be uploaded; the request's own names are the fallback.
          if (!(await falUpload.confirm(falUploadNames(e, names), falUploadCount(e) ?? undefined))) return;
          job = await app.submitCloud({ ...request, confirm_fal_upload: true }, displayName(current.name), effAspect || undefined);
        }
        if (kind === 'upscale' || kind === 'lipsync') {
          const ins = kind === 'upscale' ? [imageUrl.trim()] : [videoUrl.trim(), audioUrl.trim()];
          setJobInputs((all) => ({ ...all, [job.key]: ins.filter(Boolean) }));
        }
      }
      setPrompt('');
    } catch (e) {
      setError({ code: errorCode(e), message: errorMessage(e).replace(/^[a-z][a-z0-9_]*:\s*/, '') });
    } finally {
      setBusy(false);
    }
  }

  async function useFallback(text: string) {
    setError(null);
    // The fallback offers the first other voice route the engine lists, in its list order.
    const voice = pickOtherProviderModel(app.models, { kind: 'voice', excludeProvider: VOICE_FALLBACK_AWAY_FROM });
    if (!voice) {
      setError({ code: null, message: noModelMessage('voice') });
      return;
    }
    setBusy(true);
    try {
      await app.submitCloud({ kind: 'voice', provider: voice.provider, model: voice.model, prompt: text, params: {} }, displayName(voice.label));
    } catch (e) {
      setError({ code: errorCode(e), message: errorMessage(e).replace(/^[a-z][a-z0-9_]*:\s*/, '') });
    } finally {
      setBusy(false);
    }
  }
  const fallbackFor = kind === 'voice' ? (job: SessionJob) => void useFallback(job.prompt) : undefined;

  const reuse = (job: SessionJob) => setPrompt(job.prompt);
  const retry = (job: SessionJob) => {
    void run(job.prompt);
  };

  const cta = needsInstall
    ? (installing ? 'Installing…' : 'Install model')
    : !keyReady ? 'Add API key' : copy.cta;

  const recentAssets = kindJobs
    .filter((j) => current && j.model === current.model && j.status === 'succeeded')
    .flatMap((j) => j.assets.map((a) => ({ job: j, asset: a })))
    .slice(0, 4);

  const inputFirst = kind === 'upscale' || kind === 'lipsync';
  const compareInput = kind === 'upscale' ? imageUrl : videoUrl;
  useEffect(() => setInDims(null), [compareInput]);
  const keyless = current?.source === 'cloud' && isKeyless({ provider: current.provider, needs_key: current.entry?.needs_key });
  // Newest succeeded output for the input-first before/after strip.
  const lastOutput = inputFirst
    ? [...kindJobs].reverse().map((j) => ({ job: j, asset: j.assets[0], media: (j.assets[0]?.media || j.kind) as string })).find((x) => x.job.status === 'succeeded' && x.asset) ?? null
    : null;
  // The drawer only holds fields that are NOT already in the composer quick row
  // (aspect, video duration, music/sfx length, upscale scale, voice id live there).
  const hasSettings =
    (kind === 'image' && (resChoices.length > 0 || Boolean(iCaps?.seed))) ||
    kind === 'video' || kind === 'avatar' || kind === 'sfx' ||
    (kind === 'upscale' && uCaps?.input === 'video') ||
    (kind === 'lipsync' && Boolean(lCaps?.syncMode));
  const studioBlocked = Object.values(studioBlock).find(Boolean) ?? null;
  const disabledReason = installing ? 'Model is installing…'
    : !durationOk ? 'Pick a valid duration'
    : studioBlocked ? studioBlocked
    : null;

  // The composer is pinned to the bottom of the main column for every kind and
  // mode; only the canvas above it scrolls. Local image chat supplies its own
  // composer inside the canvas.
  return (
    <div className="content view gen">
      <section className="gen-main">
        <header className="gen-head">
          {modeOptions(kind).length > 1 ? <SegmentedControl label="Mode" value={mode} options={modeOptions(kind)} onChange={(m) => { setMode(m); if (m === 'form' || m === 'avatar') setSettingsOpen(true); }} /> : null}
          {kind === 'image' && mode === 'chat' ? (
            <Button type="button" variant="ghost" className="gen-rail-toggle" aria-expanded={railOpen} onClick={() => setRailOpen((v) => !v)}>History</Button>
          ) : null}
          <ModelPicker
            groups={groups}
            options={options}
            current={current}
            kind={kind}
            unit={UNIT[kind]}
            facts={priceFacts}
            configured={app.configured}
            onPick={(id) => {
              setSelected(id);
              window.setTimeout(() => promptRef.current?.focus(), 0);
            }}
            onManage={() => onNavigate('settings')}
          />
        </header>
        {app.modelsError ? <div style={{ margin: '0 24px 10px' }}><Notice tone="bad">{app.modelsError}</Notice></div> : null}

        <div className={`gen-canvas${railOpen ? '' : ' gen-norail'}`}>
          {localChatMode && current?.profile ? (
            needsInstall ? (
              <LocalInstallCard profile={current.profile} progress={localLive.installProgress[current.profile.id] ?? null} onInstalled={app.refreshLocalProfiles} />
            ) : (
              <LocalImageChat profile={current.profile} live={localLive} prompt={prompt} setPrompt={setPrompt} handleRef={localChat} onOpenSettings={() => onNavigate('settings')} />
            )
          ) : mode === 'workflows' ? (
            <>
              <div style={{ marginBottom: 14 }}><Notice tone="info">Workflows are coming soon. They arrive with the workflow engine.</Notice></div>
              <div className="templates" style={{ opacity: 0.5 }}>
                {WORKFLOW_TEMPLATES[kind].map(([t, d]) => (
                  <Card flush key={t} className="tpl"><span className="img" style={{ background: 'var(--fill-2)' }} /><div><b>{t}</b><small>{d}</small></div></Card>
                ))}
              </div>
            </>
          ) : (
            <Card flush className={`chat gen-chat${kind === 'image' && mode === 'chat' ? '' : ' solo'}`}>
              {kind === 'image' && mode === 'chat' ? (
                <div className="convos">
                  <h6>History</h6>
                  {kindJobs.length === 0 ? <small style={{ padding: '4px 8px', color: 'var(--ink-3)' }}>No jobs yet</small> : null}
                  {kindJobs.slice(0, 12).map((j) => (
                    <Card flush key={j.key} className="convo" onClick={() => setPrompt(j.prompt)}>
                      <b>{j.prompt || displayName(j.label)}</b>
                      <small>{displayName(j.label)} {'·'} {j.status}</small>
                    </Card>
                  ))}
                </div>
              ) : null}
              <div className="thread">
                <div className="msgs" ref={msgsRef}>
                  {thread.length === 0 ? (
                    <div className="gen-empty">
                      <EmptyState title={EMPTY_HINT[kind]} />
                      {EXAMPLES[kind].length ? (
                        <div className="gen-examples">
                          {EXAMPLES[kind].map((ex) => <Button key={ex} type="button" variant="ghost" onClick={() => { setPrompt(ex); promptRef.current?.focus(); }}>{ex}</Button>)}
                        </div>
                      ) : null}
                    </div>
                  ) : null}
                  {thread.map((job) => (
                    <div key={job.key} style={{ display: 'contents' }}>
                      <div className="you">
                        {inputFirst ? (
                          (jobInputs[job.key] ?? []).length ? (
                            (jobInputs[job.key] ?? []).map((p) => (
                              <span key={p} className="gen-inchip" title={p}>
                                {job.kind === 'upscale' ? <img src={slotUrl(p)} alt="" /> : null}
                                <b>{baseName(p)}</b>
                              </span>
                            ))
                          ) : (
                            displayName(job.label)
                          )
                        ) : (
                          job.prompt || '(no prompt recorded)'
                        )}
                      </div>
                      <div className="job">
                        <JobMeta job={job} onCancel={(j) => void app.cancel(j)} />
                        <JobOutputs job={job} onReuse={reuse} onRetry={retry} onOpenKeys={() => onNavigate('settings')} onFallback={fallbackFor} />
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            </Card>
          )}
        </div>

        {!localChatMode && hasSettings && settingsOpen ? (
          <div className="gen-drawer" role="group" aria-label="Settings">
            <Card className="form gen-drawer-form">
              {kind === 'image' ? (
                <div className="fgrid">
                  {resChoices.length ? <div><label>Resolution</label><Select label="Resolution" value={effResolution} onChange={setResolution} options={resolutionOptions} /></div> : null}
                  {iCaps?.seed ? <div><label htmlFor="g-seed">Seed</label><Input id="g-seed" inputMode="numeric" placeholder="random" value={seed} onChange={(e) => setSeed(e.target.value)} /></div> : null}
                </div>
              ) : null}
              {kind === 'video' ? (
                <div className="fgrid">
                  {resChoices.length ? <div><label>Resolution</label><Select label="Resolution" value={effResolution} onChange={setResolution} options={resolutionOptions} /></div> : <div><label>Resolution</label><Notice tone="info">Set by the model</Notice></div>}
                  {vCaps?.imageUrl !== 'none' ? (
                    <div style={{ gridColumn: '1 / -1' }}>
                      <label htmlFor="g-img">Start image URL{vCaps?.imageUrl === 'required' ? '' : ' (optional, for image-to-video)'}</label>
                      <Input id="g-img" placeholder="https://…" value={imageUrl} onChange={(e) => setImageUrl(e.target.value)} />
                    </div>
                  ) : null}
                  {vCaps && vCaps.endImage !== 'none' ? (
                    <div style={{ gridColumn: '1 / -1' }}>
                      <label htmlFor="g-endimg">End image URL{vCaps.endImage === 'required' ? '' : ' (optional)'}</label>
                      <Input id="g-endimg" placeholder="https://…" value={endImageUrl} onChange={(e) => setEndImageUrl(e.target.value)} />
                    </div>
                  ) : null}
                </div>
              ) : null}
              {kind === 'avatar' ? (
                <div className="fgrid">
                  {aCaps?.scripted ? (
                    <>
                      <div><label htmlFor="g-avid">Avatar id</label><Input id="g-avid" placeholder="HeyGen avatar id" value={avatarId} onChange={(e) => setAvatarId(e.target.value)} /></div>
                      <div><label htmlFor="g-avoice">Voice id (with a script)</label><Input id="g-avoice" placeholder="avatar default" value={voiceId} onChange={(e) => setVoiceId(e.target.value)} /></div>
                    </>
                  ) : (
                    <div style={{ gridColumn: '1 / -1' }}><label htmlFor="g-aimg">{aCaps?.imageAccepts === 'path' ? 'Portrait image path' : 'Portrait image URL'}</label><Input id="g-aimg" placeholder="https://…" value={imageUrl} onChange={(e) => setImageUrl(e.target.value)} /></div>
                  )}
                  <div style={{ gridColumn: '1 / -1' }}><label htmlFor="g-aaud">{aCaps?.audioAccepts === 'path' ? 'Audio path' : 'Audio URL'}{aCaps?.scripted ? ' (instead of a script)' : ''}</label><Input id="g-aaud" placeholder="https://…" value={audioUrl} onChange={(e) => setAudioUrl(e.target.value)} /></div>
                  {resChoices.length ? <div><label>Resolution</label><Select label="Resolution" value={effResolution} onChange={setResolution} options={resolutionOptions} /></div> : null}
                </div>
              ) : null}
              {kind === 'sfx' ? (
                <div className="fgrid">
                  <div style={{ alignSelf: 'end' }}><Checkbox checked={loop} onChange={setLoop} disabled={!sCaps?.loopParam} label="Loopable" /></div>
                  {sCaps?.mode === 'video' ? <div style={{ gridColumn: '1 / -1' }}><label htmlFor="g-svid">Video URL (video-to-sound)</label><Input id="g-svid" placeholder="https://…" value={videoUrl} onChange={(e) => setVideoUrl(e.target.value)} /></div> : null}
                </div>
              ) : null}
              {kind === 'upscale' && uCaps?.input === 'video' ? (
                <div className="fgrid">
                  <div style={{ gridColumn: '1 / -1' }}><label htmlFor="g-uvid">Video URL</label><Input id="g-uvid" placeholder="https://…" value={videoUrl} onChange={(e) => setVideoUrl(e.target.value)} /></div>
                </div>
              ) : null}
              {kind === 'lipsync' && lCaps?.syncMode ? (
                <div className="fgrid">
                  <div><label>Sync mode</label><Select label="Sync mode" value={syncMode} onChange={setSyncMode} options={[{ value: '', label: 'Auto' }, ...(lCaps?.syncMode?.values ?? []).map((s) => ({ value: s, label: s.replace('_', ' ') }))]} /></div>
                </div>
              ) : null}
            </Card>
          </div>
        ) : null}

        {!localChatMode ? (
          <Card as="section" className="gen-composer">
            {error ? <ErrorNote code={error.code} message={error.message} onOpenKeys={() => onNavigate('settings')} onFallback={kind === 'voice' ? () => void useFallback(prompt) : undefined} /> : null}
            {handoffNote ? <Notice tone="info" className="gen-handoff">{handoffNote}</Notice> : null}
            {refMax > 0 ? <ReferenceImages refs={references} max={refMax} onChange={setReferences} /> : null}
            {maskInput && references[0] ? (
              <MaskEditor src={assetUrl(references[0])} onSave={(png) => saveReferencePng(png, 'mask.png')} onChange={setMaskPath} />
            ) : null}
            {inputFirst ? (
              <>
                <div className="gen-inputs">
                  <InputSlot
                    media={kind === 'upscale' ? 'image' : 'video'}
                    label={kind === 'upscale' ? 'Input image' : 'Input video'}
                    placeholder={kind === 'upscale' ? (uCaps?.accepts === 'path' ? '/path/to/image.png on this machine' : 'Drop, choose or paste an image URL') : 'Drop, choose or paste a video URL'}
                    value={kind === 'upscale' ? imageUrl : videoUrl}
                    onChange={kind === 'upscale' ? setImageUrl : setVideoUrl}
                  />
                  {kind === 'lipsync' ? (
                    <InputSlot media="audio" label="Input audio" placeholder="Drop, choose or paste an audio URL" value={audioUrl} onChange={setAudioUrl} />
                  ) : null}
                </div>
                {lastOutput ? (
                  <div className="gen-compare">
                    <div className="gen-cmp">
                      <span className="eyebrow">Input{inDims ? ` · ${inDims.w}×${inDims.h}` : ''}</span>
                      <SlotPreview media={kind === 'upscale' ? 'image' : 'video'} value={kind === 'upscale' ? imageUrl : videoUrl} onDims={setInDims} />
                    </div>
                    <span className="gen-cmp-arrow">→</span>
                    <div className="gen-cmp">
                      <span className="eyebrow">Output{lastOutput.asset.width ? ` · ${lastOutput.asset.width}×${lastOutput.asset.height}` : ''}</span>
                      {lastOutput.media === 'image' ? (
                        <img src={assetUrl(lastOutput.asset.path)} alt={displayName(lastOutput.job.label)} />
                      ) : (
                        <MediaPlayer src={assetUrl(lastOutput.asset.path)} kind={lastOutput.media === 'audio' ? 'audio' : 'video'} compact title={displayName(lastOutput.job.label)} />
                      )}
                    </div>
                  </div>
                ) : null}
              </>
            ) : (
              <textarea className="rk-input"
                ref={promptRef}
                aria-label={kind === 'voice' || kind === 'avatar' ? 'Script' : 'Prompt'}
                placeholder={copy.placeholder}
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault();
                    void run();
                  }
                }}
              />
            )}
            {routeOpen ? (
              <RoutePanel
                kind={kind}
                pick={selected && current?.source === 'cloud' ? { provider: current.provider, model: current.model } : null}
                optionIds={options.map((o) => o.id)}
                onChoose={setSelected}
                onNavigate={onNavigate}
              />
            ) : null}
            <div className="comp-row">
              {aspects.length ? (
                <Select className="gen-select" label="Aspect ratio" value={effAspect} onChange={setAspect} options={aspects.map((a) => ({ value: a, label: a }))} />
              ) : null}
              {kind === 'video' && dSpec ? (
                <Select className="gen-select" label="Duration" value={String(effDuration ?? '')} onChange={(v) => setDuration(Number(v))} options={durationChoices(dSpec).map((d) => ({ value: String(d), label: `${d} s` }))} />
              ) : null}
              {(kind === 'music' && mCaps?.length) || (kind === 'sfx' && sCaps?.length) ? (
                <Input className="gen-mini" aria-label="Length in seconds" inputMode="decimal" placeholder="secs" value={secs} onChange={(e) => setSecs(e.target.value)} />
              ) : null}
              {kind === 'upscale' ? (
                <Select className="gen-select" label="Scale" value={String(scale)} onChange={setScale} options={UPSCALE_SCALES.map((s) => ({ value: String(s), label: `${s}×` }))} />
              ) : null}
              {kind === 'voice' && voiceIdOk ? (
                <Input className="gen-mini" aria-label="Voice id" placeholder="voice id" value={voiceId} onChange={(e) => setVoiceId(e.target.value)} />
              ) : null}
              <Button type="button" variant="ghost" aria-expanded={routeOpen} onClick={() => setRouteOpen((v) => !v)}>Why this route</Button>
              {hasSettings ? <Button type="button" variant="ghost" aria-expanded={settingsOpen} onClick={() => setSettingsOpen((v) => !v)}>Settings{settingsOpen ? ' ⌄' : ' ⌃'}</Button> : null}
              {current ? <Badge tone={current.source === 'local' || keyless ? 'ok' : undefined}>{current.source === 'local' || keyless ? 'local · free' : engineUsd != null ? `${cloudPrice} / ${UNIT[kind]}` : cloudPrice}</Badge> : null}
              <span className="gen-why">{disabledReason ?? ''}</span>
              {studioBlocked && composerAction ? (
                <Button type="button" variant="primary" className="send gen-gate" disabled={composerAction.disabled} onClick={composerAction.run}>{composerAction.label}</Button>
              ) : null}
              <Button type="button" variant="primary" className="send" disabled={busy || installing || !durationOk || Boolean(studioBlocked)} onClick={() => void run()}>{busy ? 'Sending…' : cta}</Button>
            </div>
          </Card>
        ) : null}
      </section>

      <aside className="run">
        <div className="run-head">
          <div className="eyebrow">{kind === 'image' ? 'Provider' : 'Run'}</div>
          <h3 style={{ margin: '2px 0 0', fontSize: 15, fontWeight: 600 }} title={current?.name}>{current ? displayName(current.name) : 'No model'}</h3>
        </div>
        <div className="run-body">
          {current?.source === 'local' && current.profile ? (
            <LocalPanel profile={current.profile} live={localLive} onInstalled={app.refreshLocalProfiles} />
          ) : keyless && current ? (
            <>
              <Badge tone="ok">Runs on this machine</Badge>
              <Card className="cost">
                <div><small>Estimated cost {'·'} 1 {UNIT[kind]}</small><b>Free</b></div>
                <Badge tone="ok">{app.configured.has(current.provider) ? 'ready' : 'no key needed'}</Badge>
              </Card>
              <Kv rows={[
                ['Provider', providerName(current.provider)],
                ['Model', <span className="mono" key="m">{current.model}</span>]
              ]} />
            </>
          ) : current ? (
            <>
              <Badge>Cloud provider</Badge>
              <Card className="cost">
                <div><small>Estimated cost {'·'} 1 {UNIT[kind]}</small><b>{cloudPrice}</b><PriceCaveat facts={current.source === 'cloud' ? priceFacts[`${current.provider}/${current.model}`] : undefined} /></div>
                <Badge tone={keyReady ? 'ok' : 'warn'}>{keyReady ? 'key saved' : 'no key'}</Badge>
              </Card>
              <Kv rows={[
                ['Provider', providerName(current.provider)],
                ['Model', <span className="mono" key="m">{current.model}</span>],
                ...(dSpec && effDuration ? [['Duration', `${effDuration} s`] as [string, string]] : []),
                ['Daily cap', app.spend ? `$${app.spend.cap_usd.toFixed(2)}` : 'unknown'],
                ['Spent today', app.spend ? usd(app.spend.actual_usd + app.spend.reserved_usd) : 'unknown']
              ]} />
            </>
          ) : <div className="inspector-empty"><EmptyState title="Pick a model to see its details." /></div>}
          <StudioPanels
            kind={kind}
            provider={current?.provider ?? null}
            model={current?.model ?? null}
            prompt={prompt}
            setPrompt={setPrompt}
            setParams={(patch) => setStudioParams((all) => ({ ...all, [String(patch.__panel ?? 'panel')]: Object.fromEntries(Object.entries(patch).filter(([k]) => k !== '__panel')) }))}
            setBlock={(reason) => setStudioBlock((all) => ({ ...all, panel: reason }))}
            setAction={setComposerAction}
          />
          {recentAssets.length ? (
            <div className="recent">
              <h6>Recent from this model</h6>
              <div className="strip">
                {recentAssets.map(({ job, asset }) => (
                  <div key={asset.asset_id}>{(asset.media || job.kind) === 'image' ? <img src={assetUrl(asset.path)} alt="" style={{ width: '100%', height: '100%', objectFit: 'cover' }} /> : null}</div>
                ))}
              </div>
            </div>
          ) : null}
        </div>
        <div className="run-foot">
          <div className="foot-note">
            {needsInstall ? 'Download runs in the background · you can keep working' : current?.source === 'local' || keyless ? 'Runs on this machine · no credits' : keyReady ? 'Spend is capped per day · see Models & Keys' : 'Add this provider’s key in Models & Keys'}
          </div>
        </div>
      </aside>
    </div>
  );
}

/** Compact model picker: header button + searchable combobox grouped by provider. */
function ModelPicker({ groups, options, current, kind, unit, facts, configured, onPick, onManage }: {
  groups: Array<{ key: string; title: string; items: Option[] }>;
  options: Option[];
  current: Option | undefined;
  kind: MediaKind;
  unit: string;
  facts: Record<string, ModelPriceFacts>;
  configured: Set<string>;
  onPick: (id: string) => void;
  onManage: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const btnRef = useRef<HTMLButtonElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  // Local group always first, then providers in catalog order.
  const orderedGroups = useMemo(() => [...groups.filter((g) => g.key === 'local'), ...groups.filter((g) => g.key !== 'local')], [groups]);
  const q = query.trim().toLowerCase();
  const visible = useMemo(
    () =>
      orderedGroups
        .map((g) => ({ ...g, items: g.items.filter((o) => !q || o.name.toLowerCase().includes(q) || o.provider.toLowerCase().includes(q) || o.sub.toLowerCase().includes(q)) }))
        .filter((g) => g.items.length),
    [orderedGroups, q]
  );
  const flat = useMemo(() => visible.flatMap((g) => g.items), [visible]);

  useEffect(() => {
    if (open) window.setTimeout(() => searchRef.current?.focus(), 0);
  }, [open]);

  const pick = useCallback(
    (id: string) => {
      onPick(id);
      setOpen(false);
      setQuery('');
    },
    [onPick]
  );

  const nav = useListNavigation({
    count: flat.length,
    resetKey: `${query}|${open}`,
    scrollIntoView: true,
    idPrefix: 'mpick',
    onActivate: (i) => { if (flat[i]) pick(flat[i].id); },
  });

  const ready = current ? (current.source === 'local' ? current.profile?.install_state === 'installed' : configured.has(current.provider)) : false;
  const price = current ? (current.source === 'local' || isKeyless({ provider: current.provider, needs_key: current.entry?.needs_key }) ? 'local · free' : current.entry ? priceCaption(current.entry, unit) : 'price unknown') : '';

  return (
    <div className="mpick">
      <Button
        type="button"
        variant="ghost"
        ref={btnRef}
        className="mpick-btn"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        <i className={`dot ${ready ? 'ok' : 'warn'}`} style={{ margin: 0 }} />
        <b title={current?.name}>{current ? displayName(current.name) : 'No model'}</b>
        <span className="mpick-price">{price}</span>
        <span className="mpick-chev">▾</span>
      </Button>
      {open ? (
        <Popover
          anchor={btnRef.current}
          label={`Choose a ${kind} model`}
          width={340}
          className="mpick-pop"
          onClose={() => { setOpen(false); btnRef.current?.focus(); }}
        >
        <div style={{ display: 'flex', flexDirection: 'column' }}>
          <Input ref={searchRef} className="mpick-search" placeholder="Search models…" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search models" {...nav.getInputProps()} />
          <div className="mpick-list" {...nav.getListProps()}>
            {options.length === 0 ? <div className="mpick-empty"><EmptyState title={noModelMessage(kind)} /></div> : null}
            {options.length > 0 && flat.length === 0 ? <div className="mpick-empty"><EmptyState title="No matches." /></div> : null}
            {visible.map((g) => (
              <div key={g.key} className="mpick-group">
                <div className="mpick-gt">{g.title}</div>
                {g.items.map((o) => {
                  const oReady = o.source === 'local' ? o.profile?.install_state === 'installed' : configured.has(o.provider);
                  const flatIdx = flat.indexOf(o);
                  const badge =
                    o.source === 'local'
                      ? o.profile?.install_state === 'installed'
                        ? { text: 'installed', cls: 'ok' }
                        : o.profile?.install_state === 'installing'
                          ? { text: 'installing…', cls: 'warn' }
                          : { text: `not installed${o.profile?.total_bytes ? ` · ${formatBytes(o.profile.total_bytes)}` : ''}`, cls: 'warn' }
                      : isKeyless({ provider: o.provider, needs_key: o.entry?.needs_key })
                        ? { text: 'local · free', cls: 'ok' }
                        : oReady
                          ? null
                          : { text: 'no key', cls: 'warn' };
                  const f = o.entry ? facts[`${o.provider}/${o.model}`] : undefined;
                  const estLine = o.source === 'local' || isKeyless({ provider: o.provider, needs_key: o.entry?.needs_key }) ? 'Free · local' : o.entry ? priceCaption(o.entry, unit) : 'price unknown';
                  return (
                    <Button
                      key={o.id}
                      type="button"
                      variant={flatIdx === nav.activeIndex ? 'primary' : 'ghost'}
                      {...nav.getOptionProps(flatIdx)}
                      aria-selected={current?.id === o.id}
                      tabIndex={-1}
                      className="mpick-row"
                      onClick={() => pick(o.id)}
                    >
                      <b title={o.name}>{displayName(o.name)}{current?.id === o.id ? ' ✓' : ''}</b>
                      <span className="mpick-sub">{estLine}{isAssumedRounding(f) ? ` · ${ASSUMED_ROUNDING_SENTENCE}` : ''}</span>
                      {badge ? <Badge tone={badge.cls === 'ok' ? 'ok' : 'warn'}>{badge.text}</Badge> : null}
                      {isUnconfirmedPrice(f) ? <span title={UNCONFIRMED_PRICE_SENTENCE}><Badge tone="neutral">{UNCONFIRMED_PRICE_LABEL}</Badge></span> : null}
                    </Button>
                  );
                })}
              </div>
            ))}
          </div>
          {visible.some((g) => g.items.some((o) => isUnconfirmedPrice(o.entry ? facts[`${o.provider}/${o.model}`] : undefined))) ? (
            <div className="mpick-empty" style={{ textAlign: 'left', fontSize: 11, color: 'var(--ink-3)' }}>{UNCONFIRMED_PRICE_SENTENCE}.</div>
          ) : null}
          <Button type="button" variant="ghost" size="sm" className="mpick-manage" onClick={() => { setOpen(false); onManage(); }}>
            Manage models &amp; keys →
          </Button>
        </div>
        </Popover>
      ) : null}
    </div>
  );
}

/** Neutral marker style: a quiet fill, never a warning colour. */

/**
 * A cloud estimate's caveats as a secondary line: a neutral marker when the price is unconfirmed, then the
 * sentences. Renders nothing when the engine states no caveat, so an unknown model shows the bare estimate.
 */
function PriceCaveat({ facts }: { facts: ModelPriceFacts | undefined }) {
  const notes = priceFactNotes(facts);
  if (!notes.length) return null;
  return (
    <small title={isUnconfirmedPrice(facts) ? UNCONFIRMED_PRICE_SENTENCE : undefined}>
      {isUnconfirmedPrice(facts) ? <><Badge>{UNCONFIRMED_PRICE_LABEL}</Badge>{' '}</> : null}
      {notes.join(' · ')}
    </small>
  );
}

/** Inline canvas card shown while a local image profile is not installed. */
function LocalInstallCard({ profile, progress, onInstalled }: { profile: LocalImageProfile; progress: LocalImageInstallProgress | null; onInstalled: () => Promise<void> | void }) {
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const view = installView(profile, progress);
  const active = starting || profile.install_state === 'installing';
  const pct = view.fraction === null ? null : Math.round(view.fraction * 100);
  const install = async () => {
    setError(null);
    setStarting(true);
    try {
      await installLocalImageProfile(profile.id);
      await onInstalled();
    } catch (e) {
      setError(errorMessage(e).replace(/^[a-z][a-z0-9_]*:\s*/, ''));
    } finally {
      setStarting(false);
    }
  };
  return (
    <div className="gen-install">
      <Card className={`li-install${view.failed && !active ? ' failed' : ''}`}>
        <div><b>{profile.label}</b> is not installed</div>
        <small>{formatBytes(profile.total_bytes)} download · {profile.components.filter((c) => c.required).length} files · {profile.license.name} · free after install</small>
        {active || (pct ?? 0) > 0 ? (
          <>
            <ProgressBar value={pct ?? 5} max={100} label="Install progress" />
            <small>{pct !== null ? `${pct}% · ` : ''}{view.text || 'starting'}</small>
          </>
        ) : null}
        {error || (view.failed && !active && view.error) ? <Notice tone="warn" className="li-warn">{error ?? view.error}</Notice> : null}
        <Button type="button" variant="primary" disabled={active} onClick={() => void install()}>{active ? 'Installing…' : view.failed ? 'Retry install' : 'Install model'}</Button>
      </Card>
    </div>
  );
}

/** Preview for a selected input (path or URL) in an InputSlot. */
function slotUrl(value: string): string {
  if (!value.trim()) return '';
  return URL_RE.test(value.trim()) ? value.trim() : assetUrl(value.trim().replace(/^file:\/\//, ''));
}

function SlotPreview({ media, value, onDims }: { media: 'image' | 'video' | 'audio'; value: string; onDims?: (d: { w: number; h: number } | null) => void }) {
  const [failed, setFailed] = useState(false);
  const src = slotUrl(value);
  useEffect(() => setFailed(false), [src]);
  if (!src) return <div className="slot-empty"><EmptyState title="No file" /></div>;
  if (failed) return <div className="slot-empty" title={value}><EmptyState title={baseName(value) || 'unreadable'} /></div>;
  if (media === 'image') {
    return (
      <img
        src={src}
        alt=""
        onError={() => { setFailed(true); onDims?.(null); }}
        onLoad={(e) => onDims?.({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })}
      />
    );
  }
  return <MediaPlayer src={src} kind={media} compact title={baseName(value)} />;
}

/** Input-first slot: drop zone + file picker + path/URL entry + preview. */
function InputSlot({ media, label, placeholder, value, onChange }: {
  media: 'image' | 'video' | 'audio';
  label: string;
  placeholder: string;
  value: string;
  onChange: (v: string) => void;
}) {
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const shell = useRef<HTMLDivElement | null>(null);

  // Native Tauri drag-drop carries real paths (HTML5 drop does not). Only drops over this slot count.
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let off: (() => void) | null = null;
    import('@tauri-apps/api/webviewWindow')
      .then(({ getCurrentWebviewWindow }) =>
        getCurrentWebviewWindow()
          .onDragDropEvent((ev) => {
            if (ev.payload.type === 'drop' && ev.payload.paths.length > 0 && dropInside(shell.current, ev.payload.position)) onChange(ev.payload.paths[0]);
            if (ev.payload.type === 'over') setDragOver(true);
            if (ev.payload.type === 'leave') setDragOver(false);
          })
          .then((fn) => (cancelled ? fn() : (off = fn)))
      )
      .catch(() => undefined);
    return () => {
      cancelled = true;
      off?.();
    };
  }, [onChange]);

  const accept = media === 'image' ? 'image/*' : media === 'video' ? 'video/*' : 'audio/*';
  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const f = e.dataTransfer.files?.[0] as (File & { path?: string }) | undefined;
    const text = e.dataTransfer.getData('text/plain').trim();
    if (f) onChange(f.path || f.name);
    else if (text) onChange(text);
  };
  const onPickFile = (f: File | undefined) => {
    if (f) onChange((f as File & { path?: string }).path || f.name);
  };

  return (
    <div
      ref={shell}
      className="slot2-drop"
      onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
      onDragLeave={() => setDragOver(false)}
      onDrop={onDrop}
    >
      <Card flush selected={dragOver} className="slot2">
      <input ref={fileRef} type="file" accept={accept} hidden onChange={(e) => { onPickFile(e.target.files?.[0]); e.target.value = ''; }} />
      {value.trim() ? (
        <>
          <div className="slot2-thumb"><SlotPreview media={media} value={value} /></div>
          <div className="slot2-meta">
            <b title={value}>{baseName(value)}</b>
            <Input className="slot2-path" aria-label={`${label} path or URL`} value={value} onChange={(e) => onChange(e.target.value)} />
          </div>
          <div className="slot2-acts">
            <Button type="button" variant="ghost" onClick={() => fileRef.current?.click()}>Replace…</Button>
            <IconButton label={`Clear ${label}`} onClick={() => onChange('')}>×</IconButton>
          </div>
        </>
      ) : (
        <>
          <span className="slot2-hint">Drop {media} here or</span>
          <Button type="button" variant="ghost" onClick={() => fileRef.current?.click()}>Choose file…</Button>
          <Input className="slot2-path" aria-label={`${label} path or URL`} placeholder={placeholder} value={value} onChange={(e) => onChange(e.target.value)} />
        </>
      )}
          </Card>
    </div>
  );
}

/**
 * Reference images for image models that take them. The file button uses the same hidden file input
 * as InputSlot; a name without a full path is refused, because the engine needs a URL or a real path.
 * Typed paths and URLs go through the same validation.
 */
/** True when a native drop's position (physical pixels, webview origin) lands inside `el`. */
function dropInside(el: HTMLElement | null, position: { x: number; y: number }): boolean {
  if (!el) return false;
  const scale = window.devicePixelRatio || 1;
  const x = position.x / scale;
  const y = position.y / scale;
  const box = el.getBoundingClientRect();
  return x >= box.left && x <= box.right && y >= box.top && y <= box.bottom;
}

function ReferenceImages({ refs, max, onChange }: { refs: string[]; max: number; onChange: (next: string[]) => void }) {
  const fileRef = useRef<HTMLInputElement | null>(null);
  const shell = useRef<HTMLDivElement | null>(null);
  const [draft, setDraft] = useState('');
  const [problem, setProblem] = useState<string | null>(null);
  const full = refs.length >= max;
  const latest = useRef({ refs, max, onChange });
  latest.current = { refs, max, onChange };

  // Files dropped on this group arrive with full paths through the webview (no plugin needed).
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let off: (() => void) | null = null;
    import('@tauri-apps/api/webviewWindow')
      .then(({ getCurrentWebviewWindow }) =>
        getCurrentWebviewWindow()
          .onDragDropEvent((ev) => {
            if (ev.payload.type !== 'drop' || !dropInside(shell.current, ev.payload.position)) return;
            let list = latest.current.refs;
            let refused: string | null = null;
            for (const path of ev.payload.paths) {
              const r = addReference(list, path, latest.current.max);
              if (r.error) {
                refused = r.error;
                break;
              }
              list = r.list;
            }
            setProblem(refused);
            if (list !== latest.current.refs) latest.current.onChange(list);
          })
          .then((fn) => (cancelled ? fn() : (off = fn)))
      )
      .catch(() => undefined);
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  const add = (value: string) => {
    const r = addReference(refs, value, max);
    setProblem(r.error);
    if (!r.error) {
      onChange(r.list);
      setDraft('');
    }
  };
  const onFile = (f: File | undefined) => {
    if (!f) return;
    const path = (f as File & { path?: string }).path;
    if (path) add(path);
    else setProblem(`This window did not give the full path for ${f.name}. Paste the path below.`);
  };

  return (
    <div ref={shell} className="gen-refs" role="group" aria-label="Reference images">
      <div className="gen-refs-head">
        <span className="eyebrow">Reference images · {refs.length}/{max}</span>
        <Button type="button" variant="ghost" disabled={full} onClick={() => fileRef.current?.click()}>Choose file…</Button>
        <input ref={fileRef} type="file" accept="image/*" hidden onChange={(e) => { onFile(e.target.files?.[0]); e.target.value = ''; }} />
      </div>
      {refs.length ? (
        <ul className="gen-refs-list">
          {refs.map((r) => (
            <li key={r}>
              <span title={r}>{baseName(r, 'image')}</span>
              <IconButton label={`Remove ${baseName(r, 'image')}`} onClick={() => onChange(refs.filter((x) => x !== r))}>×</IconButton>
            </li>
          ))}
        </ul>
      ) : null}
      <div className="gen-refs-add">
        <Input
          aria-label="Reference image path or URL"
          placeholder="/path/to/image.png or https://…"
          value={draft}
          disabled={full}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              add(draft);
            }
          }}
        />
        <Button type="button" variant="ghost" disabled={full || !draft.trim()} onClick={() => add(draft)}>Add</Button>
      </div>
      {problem ? <Notice tone="warn" className="li-warn">{problem}</Notice> : null}
    </div>
  );
}
