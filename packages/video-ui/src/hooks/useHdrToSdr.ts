import { useSyncExternalStore } from "react";
import { getDoctor, subscribeDoctor } from "./useEngine";

/**
 * Whether this computer can finish HDR (PQ or HLG) footage to SDR, from the engine's
 * optional `hdr_to_sdr` readiness check. `undefined` until the report loads or when the
 * engine does not report the check; callers must treat `undefined` as "unknown" and
 * disable nothing.
 */
export function useHdrToSdr(): boolean | undefined {
  const doctor = useSyncExternalStore(subscribeDoctor, getDoctor);
  const check = doctor?.readiness?.checks?.find((c) => c.id === "hdr_to_sdr");
  if (!check) return undefined;
  return (check.status ?? "").toLowerCase() === "ok";
}

/** Presets that need HDR to SDR conversion. */
export const HDR_COLOR_PRESETS = ["hdr-natural", "hdr-soft"] as const;

/** The one-line notice shown when HDR to SDR conversion is unavailable. */
export const HDR_UNAVAILABLE_NOTICE =
  "HDR to SDR conversion is not available on this computer. HDR (PQ or HLG) footage cannot be finished here.";
