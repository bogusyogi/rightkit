// @rightkit/video-ui/app: CutRight's application UI (the desktop workspace and its pages, settings, legal gate,
// onboarding gate, agent demo mode, the app's services) over the installed `VideoHost`, including its `app`
// part. A superset of the root entry: everything `@rightkit/video-ui` exports is exported here too, so an app
// imports from one place. The Tauri host is `tauriVideoHost()` on `@rightkit/video-ui/tauri`; styles are
// `@rightkit/video-ui/app.css` (which includes `styles.css`).

// The root entry: host contract, editing components, typed engine client, hooks.
export * from "../index.js";
export { videoAppHost } from "../host.js";
export type {
  VideoAppHost,
  VideoDialogFilter,
  VideoDragDropEvent,
  VideoEngineRawEvent,
  VideoPickOptions,
  VideoUpdateCheck,
} from "../host.js";

// Product name and legal facts the host supplies.
export * from "./appName.js";
export * from "./legal/legalConfig.js";

// The app shell and its pages.
export * from "./components/DesktopWorkspace.js";
export * from "./views/AssetsView.js";
export * from "./views/AutoEditView.js";
export * from "./views/BatchDelivery.js";
export * from "./views/BundleExport.js";
export * from "./views/ExportsView.js";
export * from "./views/Home.js";
export * from "./views/OnboardingGate.js";
export * from "./views/PhrasesView.js";
export * from "./views/ProgressView.js";
export * from "./views/PromptPanel.js";
export * from "./views/RecipeLibrary.js";
export * from "./views/RetentionView.js";
export * from "./views/SettingsView.js";
export * from "./views/ShortsView.js";
export * from "./views/SourcesView.js";
export * from "./views/TimelineView.js";

// Agent demo mode.
export * from "./demo/DemoSettings.js";
export * from "./demo/bindings.js";
export * from "./demo/useDemo.js";

// The app's services, over the host.
export * from "./hooks/useProgress.js";
export * from "./lib/assemble.js";
export * from "./lib/dialog.js";
export * from "./lib/engineRaw.js";
export * from "./lib/genright.js";
export * from "./lib/hostMediaAdapters.js";
export * from "./lib/mic.js";
export * from "./lib/models.js";
// 0.1.x root meaning: the model catalogue's kinds and rows (the root's lib/media names are shadowed here).
export type { ModelKind, ModelRow } from "./lib/models.js";
export * from "./lib/modelsBackend.js";
export * from "./lib/services.js";
export * from "./lib/settingsBackend.js";
export * from "./lib/shell.js";
export { Thumbnail, ProgressRow, Drawer, ErrorPanel, fileSize, type Tone } from "./ui/index.js";
export { APP_GLYPHS, type AppGlyph, type AppIconName } from "./ui/icons.js";

// 0.1.x root names the 0.2.0 root does not carry: the flat action builders and the vendored timeline pieces.
export * from "../editor/actions.js";
export { errorText } from "../editor/adapter.js";
export * from "../editor/vendor/hyperframes/PlayheadIndicator.js";
export * from "../editor/vendor/hyperframes/audioWaveformPeaks.js";
export * from "../editor/vendor/hyperframes/rulerGeometry.js";
export * from "../editor/vendor/hyperframes/useInspectorGestureTransaction.js";
