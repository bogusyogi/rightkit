export function isPillHoverTarget(target: Element | null): boolean {
  return Boolean(target?.closest(".hr-pill"));
}
