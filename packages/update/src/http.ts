// Bounded HTTP GET. All network access in this package goes through here.

export type FetchLike = (url: string, init?: { headers?: Record<string, string>; signal?: AbortSignal }) => Promise<Response>;

export const USER_AGENT = "right-update/0.1.0 (+https://github.com/bogusyogi/rightkit)";
export const FETCH_TIMEOUT_MS = 20_000;
export const MAX_RESPONSE_BYTES = 1024 * 1024;

export class HttpError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "HttpError";
  }
}

/**
 * GET a URL and return its body as text. Enforces a 20 s timeout and a
 * response size cap. Response content is data only; it is never executed.
 */
export async function fetchText(
  url: string,
  options: { fetch?: FetchLike; accept?: string; maxBytes?: number; timeoutMs?: number } = {},
): Promise<string> {
  const doFetch = options.fetch ?? (globalThis.fetch as unknown as FetchLike);
  const maxBytes = options.maxBytes ?? MAX_RESPONSE_BYTES;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? FETCH_TIMEOUT_MS);
  try {
    const response = await doFetch(url, {
      headers: { "user-agent": USER_AGENT, accept: options.accept ?? "application/json" },
      signal: controller.signal,
    });
    if (!response.ok) throw new HttpError(`GET ${url} returned HTTP ${response.status}`);
    const declared = Number(response.headers.get("content-length") ?? "0");
    if (declared > maxBytes) throw new HttpError(`GET ${url} response exceeds ${maxBytes} bytes`);
    if (!response.body) return "";
    const reader = response.body.getReader();
    const chunks: Uint8Array[] = [];
    let total = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maxBytes) {
        await reader.cancel();
        throw new HttpError(`GET ${url} response exceeds ${maxBytes} bytes`);
      }
      chunks.push(value);
    }
    return new TextDecoder().decode(Buffer.concat(chunks));
  } catch (error) {
    if ((error as Error).name === "AbortError") throw new HttpError(`GET ${url} timed out`);
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

export async function fetchJson(url: string, options: Parameters<typeof fetchText>[1] = {}): Promise<unknown> {
  const text = await fetchText(url, options);
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new HttpError(`GET ${url} did not return JSON`);
  }
}

/** Run async work with at most `limit` tasks in flight, preserving order. */
export async function mapLimit<T, R>(items: T[], limit: number, fn: (item: T) => Promise<R>): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const index = next++;
      results[index] = await fn(items[index]);
    }
  });
  await Promise.all(workers);
  return results;
}
