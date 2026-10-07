export const errText = (e: unknown): string => (e instanceof Error ? e.message : String(e));
