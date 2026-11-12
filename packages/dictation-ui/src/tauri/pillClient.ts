// The Tauri `PillClient`: the pill, clipboard popup and hint popup windows' commands and events.
import { invoke } from "@tauri-apps/api/core";
import type { PillAction, PillClient, PillEventMap, PillHistoryItem } from "../host.js";
import { eventSubscribe, tauriListenFn, type TauriClientOptions } from "./events.js";

const ACTION_COMMANDS: Record<PillAction, string> = {
  startDictation: "start_dictation",
  cancelDictation: "cancel_dictation",
  stopDictation: "pill_stop_dictation",
  stopAndSend: "pill_stop_and_send",
  openHubHome: "open_hub_home",
  openHubHistory: "open_hub_history",
  openClipboard: "open_pill_clipboard",
};

const EVENTS: { [K in keyof PillEventMap]: string } = {
  event: "pill://event",
  hover: "pill://hover",
  cursor: "pill://cursor",
  intro: "pill://intro",
  deliveryRecorded: "delivery-recorded",
  modelPhase: "local-asr-model",
  hintShow: "pill-hint://show",
  clipboardShow: "clipboard://show",
};

/** The Tauri `PillClient`. Building it touches no Tauri API; the first call does. */
export function tauriPillClient(options: TauriClientOptions = {}): PillClient {
  const listen = options.listen ?? tauriListenFn;
  return {
    on: eventSubscribe<PillEventMap>(EVENTS, listen),
    action: (name) => invoke<void>(ACTION_COMMANDS[name]),
    log: (event, message) => invoke<void>("frontend_log", { event, message }).catch(() => undefined),

    resize: (width, height) => invoke<void>("pill_resize", { width, height }),
    setHitbox: (hitbox) => invoke<void>("pill_hitbox", { x: hitbox.x, y: hitbox.y, width: hitbox.width, height: hitbox.height }),
    takeIntro: () => invoke<boolean>("take_pill_intro"),
    revealPill: () => invoke<void>("reveal_pill"),
    terminalErrorGeneration: () => invoke<number | null>("terminal_error_generation"),
    recoverTerminalError: (generation) => invoke<void>("recover_terminal_error", { generation }),

    recentHistory: () => invoke<PillHistoryItem[]>("get_recent_history"),
    showPromptHint: () => invoke<void>("show_prompt_hint"),
    copyText: (text) => invoke<void>("copy_text_to_clipboard", { text }),
    updateHistoryItem: (deliveryId, transcript) => invoke<boolean>("update_history_item", { deliveryId, transcript }),

    clipboardItems: () => invoke<PillHistoryItem[]>("get_pill_clipboard_items"),
    dismissClipboard: () => invoke<void>("dismiss_pill_clipboard"),
    resizeClipboard: (width, height) => invoke<void>("pill_clipboard_resize", { width, height }),
    clipboardReady: () => invoke<void>("pill_clipboard_ready"),

    takeHintPayload: () => invoke<{ kind?: string } | null>("take_pill_hint_payload"),
    dismissHint: () => invoke<void>("dismiss_pill_hint"),
    resizeHint: (width, height) => invoke<void>("pill_hint_resize", { width, height }),
    hintReady: () => invoke<void>("pill_hint_ready"),
    setScreenshotDestination: (destination) =>
      invoke<void>("apply_settings_patch", { patch: { screenshot_destination: destination, screenshot_prompt_selected: true } }),
  };
}
