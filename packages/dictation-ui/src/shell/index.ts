// @rightkit/dictation-ui/shell: the dictation app's shell pieces that sit around the pages (sidebar footer and update line),
// over the host's `shell` configuration and the `DictationClient` groups `license` and `sync`.

export { SidebarStatusFooter } from "./SidebarStatusFooter.js";
export { default as SidebarUpdateStatus, type SidebarUpdateState } from "./SidebarUpdateStatus.js";
export { default as FeatureReel, type FeatureReelImages } from "./FeatureReel.js";
export * from "./featureReelState.js";
export { SettingsTabs } from "./SettingsTabs.js";
export * from "./settingsSync.js";
export * from "./settingsRegistry.js";
export { usePaletteCommands, usePaletteFocusRestore, type PaletteCommand } from "./palette.js";
export { default as HoverTooltip } from "./HoverTooltip.js";
export { default as LicenseLifecycleDialog } from "./LicenseLifecycleDialog.js";
export { default as PrivacySummary } from "./PrivacySummary.js";
export { default as ModelUpdatePrompt } from "./ModelUpdatePrompt.js";
export { useTauriEvent } from "./useTauriEvent.js";
export * from "./fieldStore.js";
export { migrateLegacyPromptHint } from "./legacyPromptHint.js";
