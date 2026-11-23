/**
 * One short synthesized tone through the Web Audio API: no audio file, no dependency. The app owns the
 * frequencies and which cue plays when; this plays one `ToneSpec`.
 */
export interface ToneSpec {
  /** Pitch in Hz. */
  frequency: number;
  /** Length in seconds, attack and release included. */
  duration: number;
  /** Oscillator wave. Default `"sine"`. */
  type?: OscillatorType;
}

type AudioContextWindow = Window & {
  AudioContext?: typeof AudioContext;
  webkitAudioContext?: typeof AudioContext;
};

let context: AudioContext | null = null;

function audioContext(): AudioContext | null {
  try {
    if (context) return context;
    if (typeof window === "undefined") return null;
    const audioWindow = window as AudioContextWindow;
    const Ctor = audioWindow.AudioContext || audioWindow.webkitAudioContext;
    if (!Ctor) return null;
    context = new Ctor();
    return context;
  } catch {
    return null;
  }
}

/**
 * Plays `spec` once at `volumePercent` (0 to 100, default 50, linear gain 0 to 0.5 so a synthesized
 * tone is never loud; 0 plays nothing). A soft attack and release avoid a click. Best effort: with no
 * Web Audio, a suspended context or any audio error it returns without throwing.
 */
export function playTone(spec: ToneSpec, volumePercent = 50): void {
  const audio = audioContext();
  if (!audio) return;
  const gainValue = Math.max(0, Math.min(1, volumePercent / 100)) * 0.5;
  if (gainValue === 0) return;
  try {
    const oscillator = audio.createOscillator();
    const gain = audio.createGain();
    oscillator.type = spec.type ?? "sine";
    oscillator.frequency.value = spec.frequency;
    const now = audio.currentTime;
    gain.gain.setValueAtTime(0, now);
    gain.gain.linearRampToValueAtTime(gainValue, now + 0.005);
    gain.gain.linearRampToValueAtTime(0, now + spec.duration);
    oscillator.connect(gain);
    gain.connect(audio.destination);
    oscillator.start(now);
    oscillator.stop(now + spec.duration + 0.02);
  } catch {
    // Best effort: an autoplay-suspended context skips the tone rather than blocking the UI.
  }
}
