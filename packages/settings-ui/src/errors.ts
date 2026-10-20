/** Text of a rejection: an `Error`, a host error envelope (`{ code, message }`, as the shell host commands reject) or any value. */
export const errText = (e: unknown): string => {
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && typeof (e as { message?: unknown }).message === "string") return (e as { message: string }).message;
  return String(e);
};
