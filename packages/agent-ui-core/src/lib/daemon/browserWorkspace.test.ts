// BrowserWorkspace client tests — owned by the D3 workspace fragment.
// Uses a fake `fetch` to verify the wrapper issues correct URLs + JSON.

import { describe, expect, it, vi } from "vitest";
import { createBrowserWorkspaceClient } from "./browserWorkspace.js";

interface FetchCall {
  url: string;
  init?: RequestInit;
}

function makeFetchOk(payload: unknown) {
  return vi.fn(async (_url: string, _init?: RequestInit) => {
    return {
      ok: true,
      status: 200,
      json: async () => payload,
      text: async () => JSON.stringify(payload),
    } as unknown as Response;
  });
}

describe("createBrowserWorkspaceClient", () => {
  it("getWorkspace issues GET under /sessions/:id/browser/workspace", async () => {
    const calls: FetchCall[] = [];
    const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url, init });
      return {
        ok: true,
        status: 200,
        json: async () => ({
          paused: false,
          profileKind: "ephemeral",
          profileLabel: null,
          viewport: null,
          actionLogLen: 0,
          sessions: [],
        }),
      } as unknown as Response;
    });
    const client = createBrowserWorkspaceClient({
      baseUrl: "http://127.0.0.1:5057",
      token: "tkn",
    });
    // Replace global fetch with the mock.
    const real = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      const out = await client.getWorkspace("s1");
      expect(out.paused).toBe(false);
      expect(out.profileKind).toBe("ephemeral");
      expect(calls).toHaveLength(1);
      expect(calls[0].url).toBe("http://127.0.0.1:5057/sessions/s1/browser/workspace");
      expect(calls[0].init?.method ?? "GET").toBe("GET");
    } finally {
      globalThis.fetch = real;
    }
  });

  it("openSession serializes { url, name } and POSTs to /open", async () => {
    const calls: FetchCall[] = [];
    const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url, init });
      return {
        ok: true,
        status: 200,
        json: async () => ({ sessionId: "bs-1", url: "http://127.0.0.1:5173/" }),
      } as unknown as Response;
    });
    const client = createBrowserWorkspaceClient({
      baseUrl: "http://127.0.0.1:5057",
      token: "tkn",
    });
    const real = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      await client.openSession("s1", "http://127.0.0.1:5173/", "shared");
      expect(calls).toHaveLength(1);
      expect(calls[0].url).toBe("http://127.0.0.1:5057/sessions/s1/browser/workspace/open");
      expect(calls[0].init?.method).toBe("POST");
      const body = JSON.parse(String(calls[0].init?.body));
      expect(body).toEqual({
        url: "http://127.0.0.1:5173/",
        name: "shared",
      });
    } finally {
      globalThis.fetch = real;
    }
  });

  it("sendAnnotation uses /annotations/:id/send POST", async () => {
    const fetchMock = makeFetchOk({
      annotationId: "ann-1",
      dispatchedToSession: "s1",
      acceptedAtMs: 123,
    });
    const client = createBrowserWorkspaceClient({
      baseUrl: "http://127.0.0.1:5057",
      token: "tkn",
    });
    const real = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      const out = await client.sendAnnotation("s1", "ann-1");
      expect(out.annotationId).toBe("ann-1");
      const url = fetchMock.mock.calls[0][0] as string;
      expect(url).toBe(
        "http://127.0.0.1:5057/sessions/s1/browser/workspace/annotations/ann-1/send",
      );
      expect(fetchMock.mock.calls[0][1]?.method).toBe("POST");
    } finally {
      globalThis.fetch = real;
    }
  });

  it.each([
    "back",
    "forward",
  ] as const)("%s posts the selected browser session to the history route", async (direction) => {
    const fetchMock = makeFetchOk({ direction, browserSessionId: "browser-1" });
    const client = createBrowserWorkspaceClient({
      baseUrl: "http://127.0.0.1:5057",
      token: "tkn",
    });
    const real = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      await client[direction]("s1", "browser-1");
      expect(fetchMock.mock.calls[0][0]).toBe(
        `http://127.0.0.1:5057/sessions/s1/browser/workspace/${direction}`,
      );
      expect(fetchMock.mock.calls[0][1]?.method).toBe("POST");
      expect(JSON.parse(String(fetchMock.mock.calls[0][1]?.body))).toEqual({
        sessionId: "browser-1",
      });
    } finally {
      globalThis.fetch = real;
    }
  });

  it("raises on non-2xx with daemon error message", async () => {
    const fetchMock = vi.fn(async () => {
      return {
        ok: false,
        status: 403,
        json: async () => ({ code: "capability_denied", message: "nope" }),
      } as unknown as Response;
    });
    const client = createBrowserWorkspaceClient({
      baseUrl: "http://127.0.0.1:5057",
      token: "tkn",
    });
    const real = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      await expect(client.getWorkspace("s1")).rejects.toThrow("nope");
    } finally {
      globalThis.fetch = real;
    }
  });
});
