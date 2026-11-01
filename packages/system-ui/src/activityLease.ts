// Live-activity lease (#448). While a page showing live session activity is mounted and the
// hub window is visible, it renews a per-client lease every 4 s (engine ttl 10 s). The notch's
// session monitors then poll every 1 s; hidden or unmounted, the lease lapses and they fall back
// to their default 2 s / 3 s cadence. Renewal is skipped while hidden, so there is no release call.

import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { clearVisibleInterval, visibleInterval } from "./visible";

const RENEW_MS = 4000;

export function useActivityLease() {
  useEffect(() => {
    const client = `hub-activity-${Math.random().toString(36).slice(2)}`;
    const renew = () => {
      if (document.visibilityState === "visible") void invoke("activity_lease", { client }).catch(() => {});
    };
    renew();
    const id = visibleInterval(renew, RENEW_MS);
    return () => clearVisibleInterval(id);
  }, []);
}
