import { useCallback, useEffect, useRef, useState } from "react";

import { chooseSiteRoot, grantSiteRoot, revokeSiteRoot } from "@rightkit/document-core/state/tauriBridge";
import type { SiteRootGrant } from "./htmlInline";

/**
 * One site-root grant per HTML tab, inferred in Rust from `entryPath` (nearest enclosing project
 * directory) and revoked when the tab unmounts so it never outlives the tab.
 *
 * `siteRoot`: undefined = still resolving; null = resolved with no protocol grant (fallback).
 * `chooseRoot` opens the native folder picker (used when `markerFound` is false), swaps the grant
 * for the chosen directory and revokes the old token. A cancelled or refused pick keeps the
 * current grant. The chosen root is session-only: it lives as long as the tab's grant.
 */
export function useSiteRootGrant(entryPath: string) {
  const [siteRoot, setSiteRoot] = useState<SiteRootGrant | null | undefined>(undefined);
  const [pickingRoot, setPickingRoot] = useState(false);
  const grantedTokenRef = useRef<string | null>(null);
  const pickingRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    setSiteRoot(undefined);
    void grantSiteRoot(entryPath).then((grant) => {
      if (cancelled) {
        // Unmounted while the grant was in flight — release it rather than leak it.
        if (grant) void revokeSiteRoot(grant.token);
        return;
      }
      grantedTokenRef.current = grant?.token ?? null;
      setSiteRoot(grant);
    });
    return () => {
      cancelled = true;
      const live = grantedTokenRef.current;
      grantedTokenRef.current = null;
      if (live) void revokeSiteRoot(live);
    };
  }, [entryPath]);

  const chooseRoot = useCallback(async () => {
    if (pickingRef.current) return;
    pickingRef.current = true;
    setPickingRoot(true);
    try {
      const grant = await chooseSiteRoot(entryPath);
      if (!grant) return;
      const previous = grantedTokenRef.current;
      grantedTokenRef.current = grant.token;
      setSiteRoot(grant);
      if (previous) void revokeSiteRoot(previous);
    } finally {
      pickingRef.current = false;
      setPickingRoot(false);
    }
  }, [entryPath]);

  return { siteRoot, pickingRoot, chooseRoot };
}
