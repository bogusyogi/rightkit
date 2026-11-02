import { describe, expect, it, vi } from "vitest";
import { createWaitlistWorker, hashIp, isValidEmail, type KvStore } from "./index.js";

function memoryKv(): KvStore & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    async get(key) {
      return data.get(key) ?? null;
    },
    async put(key, value) {
      data.set(key, value);
    },
  };
}

const ctx = () => {
  const pending: Promise<unknown>[] = [];
  return { pending, waitUntil: (p: Promise<unknown>) => void pending.push(p) };
};

const post = (body: unknown, origin = "https://example.com") =>
  new Request("https://api.example.com/waitlist", {
    method: "POST",
    headers: { Origin: origin, "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.7" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });

describe("waitlist worker", () => {
  it("validates email addresses", () => {
    expect(isValidEmail("a@b.co")).toBe(true);
    expect(isValidEmail("nope")).toBe(false);
    expect(isValidEmail(42)).toBe(false);
  });

  it("stores a new signup, dedupes, and merges detail fields", async () => {
    const worker = createWaitlistWorker({
      allowedOrigins: ["https://example.com"],
      detailFields: [{ name: "current_tool", maxLength: 5 }],
    });
    const env = { WAITLIST: memoryKv(), RATELIMIT: memoryKv() };
    const first = await worker.fetch(post({ email: " Ada@Example.com ", source: "footer" }), env, ctx());
    expect(await first.json()).toEqual({ ok: true });
    expect(first.headers.get("Access-Control-Allow-Origin")).toBe("https://example.com");
    const stored = JSON.parse(env.WAITLIST.data.get("ada@example.com") ?? "{}");
    expect(stored.source).toBe("footer");
    expect(stored.ip_hash).toMatch(/^[0-9a-f]{16}$/);

    const dup = await worker.fetch(post({ email: "ada@example.com" }), env, ctx());
    expect(await dup.json()).toEqual({ ok: true, already_subscribed: true });

    const upd = await worker.fetch(post({ email: "ada@example.com", current_tool: "abcdefgh" }), env, ctx());
    expect(await upd.json()).toEqual({ ok: true, already_subscribed: true, updated: true });
    expect(JSON.parse(env.WAITLIST.data.get("ada@example.com") ?? "{}").current_tool).toBe("abcde");
  });

  it("rejects bad input, other methods, and rate-limits per IP", async () => {
    const worker = createWaitlistWorker({ allowedOrigins: ["https://example.com"], rateLimitPerHour: 2 });
    const env = { WAITLIST: memoryKv(), RATELIMIT: memoryKv() };
    expect((await worker.fetch(post("{not json"), env, ctx())).status).toBe(400);
    expect((await worker.fetch(post({ email: "x" }), env, ctx())).status).toBe(400);
    expect((await worker.fetch(post({ email: "y@example.com" }), env, ctx())).status).toBe(429);
    const get = new Request("https://api.example.com/waitlist", { method: "GET" });
    expect((await worker.fetch(get, { WAITLIST: memoryKv() }, ctx())).status).toBe(405);
  });

  it("echoes the first allowed origin for others, and allows localhost only in dev", async () => {
    const worker = createWaitlistWorker({ allowedOrigins: ["https://example.com"] });
    const env = { WAITLIST: memoryKv() };
    const other = await worker.fetch(post({ email: "a@b.co" }, "http://localhost:3000"), env, ctx());
    expect(other.headers.get("Access-Control-Allow-Origin")).toBe("https://example.com");
    const dev = await worker.fetch(post({ email: "c@b.co" }, "http://localhost:3000"), { ...env, CF_DEV: "1" }, ctx());
    expect(dev.headers.get("Access-Control-Allow-Origin")).toBe("http://localhost:3000");
  });

  it("sends the confirmation email only when configured with a key", async () => {
    const fetchMock = vi.fn(async () => new Response("{}", { status: 200 }));
    const worker = createWaitlistWorker({
      allowedOrigins: ["https://example.com"],
      email: { from: "Product <hello@example.com>", subject: "You're in", text: "t", html: "<p>h</p>" },
      fetch: fetchMock as unknown as typeof fetch,
    });
    const c1 = ctx();
    await worker.fetch(post({ email: "a@b.co" }), { WAITLIST: memoryKv() }, c1);
    await Promise.all(c1.pending);
    expect(fetchMock).not.toHaveBeenCalled();
    const c2 = ctx();
    await worker.fetch(post({ email: "a@b.co" }), { WAITLIST: memoryKv(), RESEND_API_KEY: "k" }, c2);
    await Promise.all(c2.pending);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("hashes IPs with and without a secret", async () => {
    expect(await hashIp("1.2.3.4")).not.toBe(await hashIp("1.2.3.4", "s"));
  });
});
