import { useEffect, useState } from "react";

/* Focus and dismiss hooks live in @rightkit/platform-ui/headless (no app-shell peer needed). */
export { useFocusTrap, useLatest, useModalDismiss } from "@rightkit/platform-ui/headless";

/** Media-query subscription (narrow layout, reduced motion). */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => (typeof window !== "undefined" && window.matchMedia ? window.matchMedia(query).matches : false));
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia(query);
    const on = () => setMatches(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [query]);
  return matches;
}
