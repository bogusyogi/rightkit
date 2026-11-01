import { CaptionControls, type CaptionButtonClasses } from "@rightkit/react-shell/react";

/** Native window bridge consumed by the published app-shell frame. */
export const tauriWindowControls = {
  minimize: async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().minimize();
  },
  toggleMaximize: async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().toggleMaximize();
  },
  close: async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().close();
  },
};

/**
 * the host app windows are never maximizable (tauri.windows.conf.json maximizable:false, and the hub builder),
 * but the shared CaptionControls always draws a Maximise button. Hide it; the important modifier beats
 * shell.css's unlayered `.rk-caption__btn { display: flex }`.
 */
export const noMaximizeCaptionClasses: CaptionButtonClasses = { maximize: "hr-caption-hidden" };

/** Windows/Linux Minimise and Close caption buttons for the frameless setup windows (macOS keeps its traffic lights). */
export function SetupCaptionControls({ className }: { className?: string }) {
  return <CaptionControls adapter={tauriWindowControls} buttonClasses={noMaximizeCaptionClasses} className={className} />;
}
