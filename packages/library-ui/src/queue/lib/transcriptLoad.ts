import type { TranscriptDetail, TranscriptVariant } from "../contract";

// Injected so the load chain stays testable without the Tauri runtime.
export type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

// The slim reply carries only a preview; fetch the full text body and merge it
// in. A failed variant fetch keeps the preview text.
export async function loadTranscriptDetail(invokeFn: InvokeFn, id: string): Promise<TranscriptDetail> {
  const slim = await invokeFn<TranscriptDetail>("get_transcript", { id });
  const v = await invokeFn<TranscriptVariant>("get_transcript_variant", {
    id,
    kind: "text",
  }).catch(() => null);
  return { ...slim, text: v?.text ?? slim.text };
}
