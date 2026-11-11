// @rightkit/dictation-ui/pill: the recording pill and its two popups (hint, mini clipboard). Each
// runs in its own window; the host calls `configureDictationHost({ pill })` there first. This entry
// imports nothing from the pages, so a pill window's bundle holds the pill only.

export { configureDictationHost, dictationStorageKey, hostAppName } from "../host.js";
export type {
  DeliveryTarget,
  DictationHostConfig,
  DictationStorageKey,
  PillAction,
  PillClient,
  PillEventMap,
  PillHistoryItem,
  PillHitbox,
  Subscribe,
  Unsubscribe,
} from "../host.js";
export { default as PillOverlay } from "./PillOverlay.js";
export { default as HintPopup } from "./HintPopup.js";
export { default as MiniClipboardPopup } from "./MiniClipboardPopup.js";
export { default as MiniClipboard } from "./MiniClipboard.js";
export * from "./MiniClipboard.js";
export * from "./PillIcon.js";
export * from "./hoverTarget.js";
export * from "./pillBars.js";
export * from "./pillStyles.js";
export * from "./pillTooltip.js";
export * from "./usePillAnimation.js";
export * from "./usePillEvents.js";
export * from "./usePillGeometry.js";
export { listenPill } from "./pillHost.js";
