/**
 * Waitlist worker: captures email signups in a Cloudflare KV namespace and (optionally) sends a
 * confirmation email through Resend. KV is the source of truth; the email is best-effort and is
 * skipped when `RESEND_API_KEY` is not configured.
 *
 * Endpoint:  POST /waitlist (any path; the worker answers every POST on its route)
 * Body:      { "email": "user@example.com", "source"?: string, ...configured detail fields }
 * Response:  { "ok": true }                                             (new signup)
 *            { "ok": true, "already_subscribed": true }                 (duplicate)
 *            { "ok": true, "already_subscribed": true, "updated": true } (details saved on a duplicate)
 *            { "error": "invalid_email" | "rate_limited" | "invalid_json" | "method_not_allowed" }
 */

/** The subset of the Workers KV API the worker uses. */
export interface KvStore {
  get(key: string): Promise<string | null>;
  put(key: string, value: string, options?: { expirationTtl?: number }): Promise<void>;
}

export interface WaitlistEnv {
  WAITLIST: KvStore;
  RATELIMIT?: KvStore;
  RESEND_API_KEY?: string;
  /** When set, client IPs are stored as an HMAC with this secret; otherwise as a truncated SHA-256. */
  IP_HASH_SECRET?: string;
  /** "1" allows any `http://localhost:*` origin (local development). */
  CF_DEV?: string;
}

export interface WaitlistExecutionContext {
  waitUntil(promise: Promise<unknown>): void;
}

export interface WaitlistEmail {
  /** e.g. `"Product <hello@example.com>"`. */
  from: string;
  replyTo?: string;
  subject: string;
  text: string;
  html: string;
}

export interface WaitlistDetailField {
  /** JSON body field, stored under the same name. */
  name: string;
  maxLength: number;
}

export interface WaitlistConfig {
  /** Exact origins allowed by CORS. The first one is echoed for disallowed origins. */
  allowedOrigins: readonly string[];
  /** Optional free-text fields saved with the signup and merged into an existing one. */
  detailFields?: readonly WaitlistDetailField[];
  /** Store the body's `source` (trimmed to 40 chars, default `"site"`). Default true. */
  recordSource?: boolean;
  /** Requests per IP per hour before `rate_limited`. Default 10. Needs the `RATELIMIT` binding. */
  rateLimitPerHour?: number;
  /** Confirmation email; omit to never send one. */
  email?: WaitlistEmail;
  /** Resend endpoint (tests point it elsewhere). */
  resendUrl?: string;
  /** fetch used for Resend (tests inject one). */
  fetch?: typeof fetch;
}

export type WaitlistRecord = {
  email: string;
  signed_up_at: string;
  source?: string;
  country: string;
  user_agent: string;
  referrer: string;
  ip_hash: string;
  details_updated_at?: string;
  [detail: string]: string | undefined;
};

export function isValidEmail(s: unknown): s is string {
  if (typeof s !== "string") return false;
  const trimmed = s.trim();
  if (trimmed.length < 5 || trimmed.length > 254) return false;
  return /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/.test(trimmed);
}

export function isOriginAllowed(origin: string | null, config: WaitlistConfig, env: WaitlistEnv): boolean {
  if (!origin) return false;
  if (config.allowedOrigins.includes(origin)) return true;
  return env.CF_DEV === "1" && origin.startsWith("http://localhost:");
}

function corsHeaders(origin: string | null, config: WaitlistConfig, env: WaitlistEnv): Record<string, string> {
  const allow = isOriginAllowed(origin, config, env) && origin ? origin : (config.allowedOrigins[0] ?? "");
  return {
    "Access-Control-Allow-Origin": allow,
    "Access-Control-Allow-Methods": "POST, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type",
    "Access-Control-Max-Age": "86400",
    Vary: "Origin",
  };
}

function cleanText(value: unknown, maxLength: number): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed ? trimmed.slice(0, maxLength) : undefined;
}

export async function hashIp(ip: string, secret?: string): Promise<string> {
  const encoder = new TextEncoder();
  let bytes: ArrayBuffer;
  if (secret) {
    const key = await crypto.subtle.importKey("raw", encoder.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, [
      "sign",
    ]);
    bytes = await crypto.subtle.sign("HMAC", key, encoder.encode(ip));
  } else {
    bytes = await crypto.subtle.digest("SHA-256", encoder.encode(ip));
  }
  return Array.from(new Uint8Array(bytes))
    .slice(0, 8)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

async function sendConfirmationEmail(config: WaitlistConfig, toEmail: string, apiKey?: string): Promise<void> {
  const email = config.email;
  if (!email || !apiKey) return;
  const doFetch = config.fetch ?? fetch;
  try {
    const res = await doFetch(config.resendUrl ?? "https://api.resend.com/emails", {
      method: "POST",
      headers: { Authorization: `Bearer ${apiKey}`, "Content-Type": "application/json" },
      body: JSON.stringify({
        from: email.from,
        to: [toEmail],
        ...(email.replyTo ? { reply_to: email.replyTo } : {}),
        subject: email.subject,
        html: email.html,
        text: email.text,
        tags: [{ name: "type", value: "waitlist_confirmation" }],
      }),
    });
    if (!res.ok) console.error(`Resend send failed: status=${res.status}`);
  } catch (err) {
    console.error(`Resend send threw: ${(err as Error).message}`);
  }
}

/** Builds the worker's `fetch` handler for one site. Export it as the worker's default. */
export function createWaitlistWorker(config: WaitlistConfig) {
  const limit = config.rateLimitPerHour ?? 10;
  const recordSource = config.recordSource ?? true;
  return {
    async fetch(request: Request, env: WaitlistEnv, ctx: WaitlistExecutionContext): Promise<Response> {
      const origin = request.headers.get("Origin");
      const json = (body: unknown, status: number) =>
        new Response(JSON.stringify(body), {
          status,
          headers: { "Content-Type": "application/json", ...corsHeaders(origin, config, env) },
        });

      if (request.method === "OPTIONS") {
        return new Response(null, { status: 204, headers: corsHeaders(origin, config, env) });
      }
      if (request.method !== "POST") return json({ error: "method_not_allowed" }, 405);

      const ip = request.headers.get("CF-Connecting-IP") || "unknown";
      if (env.RATELIMIT) {
        const rateKey = `rl:${ip}`;
        const count = Number.parseInt((await env.RATELIMIT.get(rateKey)) || "0", 10);
        if (count >= limit) return json({ error: "rate_limited" }, 429);
        await env.RATELIMIT.put(rateKey, String(count + 1), { expirationTtl: 3600 });
      }

      let payload: Record<string, unknown>;
      try {
        payload = (await request.json()) as Record<string, unknown>;
      } catch {
        return json({ error: "invalid_json" }, 400);
      }
      if (!payload || typeof payload !== "object" || !isValidEmail(payload.email)) {
        return json({ error: "invalid_email" }, 400);
      }
      const normalized = payload.email.trim().toLowerCase();

      const details: Record<string, string> = {};
      for (const field of config.detailFields ?? []) {
        const value = cleanText(payload[field.name], field.maxLength);
        if (value) details[field.name] = value;
      }

      const existing = await env.WAITLIST.get(normalized);
      if (existing) {
        if (Object.keys(details).length > 0) {
          const updated = { ...(JSON.parse(existing) as WaitlistRecord), ...details, details_updated_at: new Date().toISOString() };
          await env.WAITLIST.put(normalized, JSON.stringify(updated));
          return json({ ok: true, already_subscribed: true, updated: true }, 200);
        }
        return json({ ok: true, already_subscribed: true }, 200);
      }

      const record: WaitlistRecord = {
        email: normalized,
        signed_up_at: new Date().toISOString(),
        ...(recordSource ? { source: typeof payload.source === "string" ? payload.source.slice(0, 40) : "site" } : {}),
        country: request.headers.get("CF-IPCountry") || "unknown",
        user_agent: (request.headers.get("User-Agent") || "").slice(0, 200),
        referrer: (request.headers.get("Referer") || "").slice(0, 200),
        ip_hash: await hashIp(ip, env.IP_HASH_SECRET),
        ...details,
      };
      await env.WAITLIST.put(normalized, JSON.stringify(record));
      ctx.waitUntil(sendConfirmationEmail(config, normalized, env.RESEND_API_KEY));
      return json({ ok: true }, 200);
    },
  };
}
