export function isPillHoverTarget(target: Element | null): boolean {
  return Boolean(target?.closest("[data-pill-surface]"));
}
