// Web Audio synthesizer for state-transition feedback tones.
// No bundled WAVs - short tone bursts generated on demand.
// Volume + on/off controlled by settings.json (Pass 2 toggles).

type SoundKind = "armed" | "recording_start" | "recording_stop" | "pasted" | "error";
type AudioContextWindow = Window & {
  AudioContext?: typeof AudioContext;
  webkitAudioContext?: typeof AudioContext;
};

let ctx: AudioContext | null = null;

function getCtx(): AudioContext | null {
  try {
    if (!ctx) {
      const audioWindow = window as AudioContextWindow;
      const Ctor =
        typeof window !== "undefined"
          ? (audioWindow.AudioContext || audioWindow.webkitAudioContext)
          : null;
      if (!Ctor) return null;
      ctx = new Ctor();
    }
    return ctx;
  } catch {
    return null;
  }
}

interface ToneSpec {
  frequency: number;
  duration: number;
  type: OscillatorType;
}

const TONES: Record<SoundKind, ToneSpec> = {
  armed: { frequency: 660, duration: 0.05, type: "sine" },
  recording_start: { frequency: 880, duration: 0.08, type: "sine" },
  recording_stop: { frequency: 550, duration: 0.08, type: "sine" },
  pasted: { frequency: 990, duration: 0.06, type: "sine" },
  error: { frequency: 220, duration: 0.18, type: "square" },
};

/**
 * The cue for an `app-state-changed` state, or null for states that stay silent. A paste whose
 * arrival could not be confirmed (`paste_sent_unconfirmed`, common on Windows) was still sent, so
 * it plays the same cue as a confirmed one.
 */
export function soundForAppState(state: string | undefined): SoundKind | null {
  switch (state) {
    case "recording":
      return "recording_start";
    case "transcribing":
      return "recording_stop";
    case "pasted":
    case "paste_sent_unconfirmed":
      return "pasted";
    case "error":
      return "error";
    default:
      return null;
  }
}

export function playSound(kind: SoundKind, volumePercent = 50): void {
  const audio = getCtx();
  if (!audio) return;
  const spec = TONES[kind];
  if (!spec) return;
  // Volume mapped 0..100 → linear gain 0..0.5 (cap so synthesized tones aren't loud)
  const gainValue = Math.max(0, Math.min(1, volumePercent / 100)) * 0.5;
  if (gainValue === 0) return;
  try {
    const osc = audio.createOscillator();
    const gain = audio.createGain();
    osc.type = spec.type;
    osc.frequency.value = spec.frequency;
    const now = audio.currentTime;
    // Soft attack + release to avoid click on start/stop.
    gain.gain.setValueAtTime(0, now);
    gain.gain.linearRampToValueAtTime(gainValue, now + 0.005);
    gain.gain.linearRampToValueAtTime(0, now + spec.duration);
    osc.connect(gain);
    gain.connect(audio.destination);
    osc.start(now);
    osc.stop(now + spec.duration + 0.02);
  } catch {
    // Best-effort - if audio context is suspended (autoplay policy etc.),
    // we silently skip rather than blocking the UI.
  }
}
