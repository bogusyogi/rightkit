import { afterEach, describe, expect, it, vi } from "vitest";

import type { DaemonConfig, ProtocolEvent } from "../types.js";
import { openEventStream, replayEventPages } from "./messages-stream-readiness.js";

const config = { url: "http://daemon.test/", token: "tok" } as DaemonConfig;

function ev(seq: number, $kind = "assistantDelta"): ProtocolEvent {
  return { $kind, seq, sessionId: "s1", createdAt: "2026-10-17T00:00:00Z" } as ProtocolEvent;
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function sse(frames: ProtocolEvent[]): Response {
  const text = frames.map((frame) => `data: ${JSON.stringify(frame)}\n\n`).join("");
  return new Response(text, { status: 200, headers: { "content-type": "text/event-stream" } });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("replayEventPages", () => {
  it("pages after=next until complete and applies events in order", async () => {
    const urls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string) => {
        urls.push(input);
        const after = new URL(input).searchParams.get("after");
        if (after === "5") {
          return json({ events: [ev(6), ev(7)], next: 7, complete: false, highWaterMark: 9 });
        }
        return json({ events: [ev(8), ev(9)], next: 9, complete: true, highWaterMark: 9 });
      }),
    );
    const seen: number[] = [];
    const cursor = await replayEventPages(config, "s1", 5, (event) => seen.push(event.seq));
    expect(seen).toEqual([6, 7, 8, 9]);
    expect(cursor).toBe(9);
    expect(urls.map((url) => new URL(url).searchParams.get("after"))).toEqual(["5", "7"]);
    expect(urls[0]).toContain("/sessions/s1/events/replay");
  });
});

describe("openEventStream resync", () => {
  it("replays on streamResyncRequired then reconnects /events?after=next", async () => {
    vi.useFakeTimers();
    const calls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string) => {
        calls.push(input);
        const url = new URL(input);
        if (url.pathname.endsWith("/events/replay")) {
          return json({ events: [ev(6), ev(7)], next: 7, complete: true, highWaterMark: 7 });
        }
        if (calls.filter((call) => call.includes("/events?")).length === 1) {
          return sse([ev(5), ev(999, "streamResyncRequired")]);
        }
        return new Response(new ReadableStream(), { status: 200 });
      }),
    );
    const seen: number[] = [];
    const stop = openEventStream(config, "s1", 4, (event) => seen.push(event.seq), () => undefined);
    await vi.advanceTimersByTimeAsync(1500);
    stop();
    expect(seen).toEqual([5, 6, 7]);
    const replayIdx = calls.findIndex((call) => call.includes("/events/replay"));
    expect(replayIdx).toBeGreaterThan(0);
    expect(new URL(calls[replayIdx]).searchParams.get("after")).toBe("5");
    const reconnect = calls.slice(replayIdx + 1).find((call) => call.includes("/events?"));
    expect(reconnect && new URL(reconnect).searchParams.get("after")).toBe("7");
  });
});
