// Settings types - mirrors the Rust `SettingsBlob` shape. All fields optional;
// the backend fills defaults when a key is unset. Keep aligned with
// src-tauri/src/settings.rs.

export interface SettingsBlob {
  hotkey_toggle?: string;
  hotkey_cancel?: string;
  hotkey_open_hub?: string;
  input_device?: string;
  sound_effects_on?: boolean;
  sound_effects_volume?: number;
  media_ducking_on?: boolean;
  // Two spec-locked values: `always` and `recording_only`. The legacy
  // `during_dictation` alias is accepted by the backend and maps to
  // `recording_only`. Default for new installs is `always`
  // (the native settings default).
  pill_visibility?: "always" | "recording_only" | "during_dictation";
  audio_conditioning?: boolean;
  asr_backend?: "parakeet" | "parakeet-unified" | "parakeet-tdt" | "whisper" | "whisper-cli";
  // "auto" = Parakeet auto-detect (free); a lang code = lock to Whisper (Pro).
  dictation_language?: string;
  transcript_cleanup?: "clean" | "aggressive";
  ai_polish_enabled?: boolean;
  ai_polish_mode?: "app";
  // Text insertion method. `paste` = clipboard + ⌘V (default, works in every app
  // incl. Electron/Chromium). `keystroke` = synthetic typing for apps that
  // mishandle programmatic paste. Mirrors DEFAULT_DELIVERY_METHOD in core settings.
  delivery_method?: "paste" | "keystroke";
  screenshot_destination?: "clipboard" | "disk" | "both";
  screenshot_prompt_selected?: boolean;
  asr_compute_profile?: "automatic" | "neural_engine" | "cpu_gpu" | "cpu_only";
  asr_detailed_telemetry?: boolean;
  diagnostic_audio_capture?: boolean;
  diagnostic_unredacted_logs?: boolean;
  streaming_insertion?: boolean;
  history_retention?: "forever" | "7_days" | "30_days" | "90_days" | "1_year";
  push_to_talk?: boolean;
  // Windows single-key PTT.
  push_to_talk_key?: "lalt" | "ralt" | "rctrl";
  // macOS single-modifier PTT. fn remains stop-and-send companion.
  mac_push_to_talk_key?: "ctrl" | "lalt" | "ralt";
  middle_mouse_recording?: boolean;
  // Find-replace text expansion: trigger → replacement. Patched as an object;
  // backend `apply_settings_patch` accepts a "snippets" object (settings.rs).
  snippets?: Record<string, string>;
  // Deterministic text replacements applied at the local polish layer: from
  // phrase → to text. Patched as an object; backend `apply_settings_patch`
  // accepts a "replacements" object (settings.rs).
  replacements?: Record<string, string>;
}

export type PatchValue = string | boolean | number | Record<string, string>;

// Stable patch callback shared by section components. Sections fire-and-forget
// (onChange handlers), so the returned promise is typically ignored.
export type PatchFn = (key: keyof SettingsBlob, value: PatchValue) => Promise<void>;
