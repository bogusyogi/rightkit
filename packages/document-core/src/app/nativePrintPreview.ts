import { invoke } from "@tauri-apps/api/core";

export interface NativePrintPreviewPayload {
  html: string;
  shadowHtml: string | null;
}

export function openPrintPreviewWindow(payload: NativePrintPreviewPayload): Promise<boolean> {
  return invoke<boolean>("open_print_preview_window", { payload });
}

export function takePrintPreviewPayload(): Promise<NativePrintPreviewPayload> {
  return invoke<NativePrintPreviewPayload>("take_print_preview_payload");
}
