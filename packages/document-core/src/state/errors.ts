export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  if (error && typeof error === "object") {
    const maybe = error as { message?: unknown; error?: unknown; code?: unknown };
    if (typeof maybe.message === "string") return maybe.message;
    if (typeof maybe.error === "string") return maybe.error;
    if (typeof maybe.code === "string") return maybe.code;
    try {
      return JSON.stringify(error);
    } catch {
      /* fall through */
    }
  }
  return String(error);
}
