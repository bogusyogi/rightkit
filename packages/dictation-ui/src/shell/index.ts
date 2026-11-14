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
