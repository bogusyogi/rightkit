export type PillTooltipSource = "pill" | "pill_clipboard" | "pill_hint";

export function pillTooltipProps(_source: PillTooltipSource, text: string) {
  return {
    title: text,
  };
}
