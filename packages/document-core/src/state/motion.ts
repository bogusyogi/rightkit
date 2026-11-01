function prefersReducedMotion(): boolean {
  // The in-app Motion setting (data-motion on <html>) wins over the OS preference.
  const choice = typeof document !== "undefined" ? document.documentElement.dataset.motion : undefined;
  if (choice === "reduce") return true;
  if (choice === "full") return false;
  const matchMedia = typeof window !== "undefined" ? window.matchMedia : globalThis.matchMedia;
  return matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;
}

export function scrollBehaviorForMotion(): ScrollBehavior {
  return prefersReducedMotion() ? "auto" : "smooth";
}
