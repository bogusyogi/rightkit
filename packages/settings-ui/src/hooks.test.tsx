// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { StoredLicense } from "@rightkit/license";
import { useDoctor, useLicenseSection } from "./index.js";
import type { DoctorCheck, DoctorResult, LicenseClientLike } from "./index.js";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const active = { status: "active", deviceId: "d", licenseKind: "perpetual", validUntil: null, entitlements: { tier: "pro" } } as unknown as StoredLicense;
const make = (over: Partial<LicenseClientLike> = {}) => {
  const client: LicenseClientLike = { status: async () => null, activate: vi.fn(async () => active), deactivate: vi.fn(async () => ({})), ...over };
  return client;
};

describe("useLicenseSection", () => {
  it("reads the licence on mount (status or load) and reports pro", async () => {
    const { result } = renderHook(() => useLicenseSection({ client: make({ status: async () => active }) }));
    await waitFor(() => expect(result.current.pro).toBe(true));
    expect(result.current.license).toBe(active);
    expect(result.current.tier).toBe("pro");
    const viaLoad = renderHook(() => useLicenseSection({ client: { activate: async () => active, deactivate: async () => ({}), load: async () => active } }));
    await waitFor(() => expect(viaLoad.result.current.pro).toBe(true));
  });
  it("an active licence below the pro tier is licensed but not pro", async () => {
    const free = { ...active, entitlements: { tier: "free" } } as unknown as StoredLicense;
    const { result } = renderHook(() => useLicenseSection({ client: make({ status: async () => free }) }));
    await waitFor(() => expect(result.current.license).toBe(free));
    expect(result.current.pro).toBe(false);
  });
  it("is not pro without a licence; a client with no reader means none", async () => {
    const { result } = renderHook(() => useLicenseSection({ client: { activate: async () => active, deactivate: async () => ({}) } }));
    expect(result.current.pro).toBe(false);
    expect(await result.current.refresh()).toBeNull();
  });
  it("surfaces a failed read as error; refresh recovers and clears it", async () => {
    let fail = true;
    const client = make({ status: async () => { if (fail) throw new Error("read failed"); return active; } });
    const { result } = renderHook(() => useLicenseSection({ client }));
    await waitFor(() => expect(result.current.error).toBe("read failed"));
    fail = false;
    await act(async () => { await result.current.refresh(); });
    expect(result.current.error).toBeNull();
    expect(result.current.pro).toBe(true);
  });
  it("activate success: sets license, fires onChange, returns the licence", async () => {
    const onChange = vi.fn();
    const client = make();
    const { result } = renderHook(() => useLicenseSection({ client, onChange }));
    let out: StoredLicense | null = null;
    await act(async () => { out = await result.current.activate("KEY"); });
    expect(client.activate).toHaveBeenCalledWith("KEY");
    expect(out).toBe(active);
    expect(result.current.pro).toBe(true);
    expect(result.current.error).toBeNull();
    expect(result.current.busy).toBe(false);
    expect(onChange).toHaveBeenCalledWith(active);
  });
  it("activate ignores a blank key", async () => {
    const client = make();
    const { result } = renderHook(() => useLicenseSection({ client }));
    await act(async () => { expect(await result.current.activate("  ")).toBeNull(); });
    expect(client.activate).not.toHaveBeenCalled();
  });
  it("activate failure: thrown error is error, returns null, no onChange", async () => {
    const onChange = vi.fn();
    const client = make({ activate: async () => { throw new Error("offline"); } });
    const { result } = renderHook(() => useLicenseSection({ client, onChange }));
    await act(async () => { expect(await result.current.activate("K")).toBeNull(); });
    expect(result.current.error).toBe("offline");
    expect(result.current.pro).toBe(false);
    expect(onChange).not.toHaveBeenCalled();
  });
  it("activate with a rejected licence: error is its message or the fallback, onChange still fires", async () => {
    const onChange = vi.fn();
    const rejected = { status: "invalid", message: "Key revoked" } as StoredLicense;
    const { result, rerender } = renderHook((p: { c: LicenseClientLike }) => useLicenseSection({ client: p.c, onChange, notAcceptedMessage: "Nope." }), { initialProps: { c: make({ activate: async () => rejected }) } });
    await act(async () => { await result.current.activate("K"); });
    expect(result.current.error).toBe("Key revoked");
    expect(onChange).toHaveBeenCalledWith(rejected);
    rerender({ c: make({ activate: async () => ({ status: "failed" } as StoredLicense) }) });
    await act(async () => { await result.current.activate("K"); });
    expect(result.current.error).toBe("Nope.");
  });
  it("activating is true while the call is in flight", async () => {
    let release!: (l: StoredLicense) => void;
    const client = make({ activate: () => new Promise((r) => { release = r; }) });
    const { result } = renderHook(() => useLicenseSection({ client }));
    let p!: Promise<unknown>;
    act(() => { p = result.current.activate("K"); });
    expect(result.current.activating).toBe(true);
    expect(result.current.busy).toBe(true);
    await act(async () => { release(active); await p; });
    expect(result.current.activating).toBe(false);
  });
  it("deactivate(): runs at once regardless of confirmDeactivate, then clears licence and fires onChange(null)", async () => {
    const onChange = vi.fn();
    const client = make({ status: async () => active });
    const { result } = renderHook(() => useLicenseSection({ client, onChange, confirmDeactivate: true }));
    await waitFor(() => expect(result.current.pro).toBe(true));
    await act(async () => { await result.current.deactivate(); });
    expect(client.deactivate).toHaveBeenCalledTimes(1);
    expect(result.current.license).toBeNull();
    expect(onChange).toHaveBeenCalledWith(null);
  });
  it("requestDeactivate with confirmation on: first call arms, second confirms, cancel disarms", async () => {
    const client = make({ status: async () => active });
    const { result } = renderHook(() => useLicenseSection({ client }));
    await waitFor(() => expect(result.current.pro).toBe(true));
    act(() => result.current.requestDeactivate());
    expect(result.current.confirmPending).toBe(true);
    expect(client.deactivate).not.toHaveBeenCalled();
    act(() => result.current.cancelDeactivate());
    expect(result.current.confirmPending).toBe(false);
    act(() => result.current.requestDeactivate());
    await act(async () => { result.current.requestDeactivate(); });
    await waitFor(() => expect(client.deactivate).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(result.current.license).toBeNull());
    expect(result.current.confirmPending).toBe(false);
  });
  it("requestDeactivate confirmation disarms itself after 4 seconds", async () => {
    const client = make({ status: async () => active });
    const { result } = renderHook(() => useLicenseSection({ client }));
    await waitFor(() => expect(result.current.pro).toBe(true));
    vi.useFakeTimers();
    act(() => result.current.requestDeactivate());
    expect(result.current.confirmPending).toBe(true);
    act(() => { vi.advanceTimersByTime(4001); });
    expect(result.current.confirmPending).toBe(false);
    expect(client.deactivate).not.toHaveBeenCalled();
  });
  it("requestDeactivate with confirmDeactivate false deactivates on the first call", async () => {
    const client = make({ status: async () => active });
    const { result } = renderHook(() => useLicenseSection({ client, confirmDeactivate: false }));
    await waitFor(() => expect(result.current.pro).toBe(true));
    await act(async () => { result.current.requestDeactivate(); });
    await waitFor(() => expect(client.deactivate).toHaveBeenCalledTimes(1));
    expect(result.current.confirmPending).toBe(false);
  });
  it("beforeDeactivate runs first; a throw aborts, shows the error and keeps the licence", async () => {
    const order: string[] = [];
    const client = make({ status: async () => active, deactivate: vi.fn(async () => { order.push("client"); return {}; }) });
    const onChange = vi.fn();
    const { result, rerender } = renderHook((p: { b: () => void }) => useLicenseSection({ client, onChange, beforeDeactivate: p.b }), { initialProps: { b: () => { order.push("before"); } } });
    await waitFor(() => expect(result.current.pro).toBe(true));
    await act(async () => { await result.current.deactivate(); });
    expect(order).toEqual(["before", "client"]);
    rerender({ b: () => { throw new Error("bridge busy"); } });
    onChange.mockClear();
    await act(async () => { await result.current.activate("K"); }); // back to active
    (client.deactivate as ReturnType<typeof vi.fn>).mockClear();
    await act(async () => { await result.current.deactivate(); });
    expect(client.deactivate).not.toHaveBeenCalled();
    expect(result.current.error).toBe("bridge busy");
    expect(result.current.pro).toBe(true);
    expect(onChange).toHaveBeenCalledTimes(1); // the re-activation only
  });
  it("a failed client.deactivate keeps the licence and sets error", async () => {
    const client = make({ status: async () => active, deactivate: async () => { throw new Error("server down"); } });
    const { result } = renderHook(() => useLicenseSection({ client }));
    await waitFor(() => expect(result.current.pro).toBe(true));
    await act(async () => { await result.current.deactivate(); });
    expect(result.current.error).toBe("server down");
    expect(result.current.pro).toBe(true);
  });
});

describe("useDoctor", () => {
  const ok = (detail = "fine"): DoctorResult => ({ ok: true, detail });
  const mk = () => {
    const checks: DoctorCheck[] = [
      { id: "a", label: "A", run: vi.fn(async () => ok()) },
      { id: "b", label: "B", run: vi.fn(async () => ({ ok: false, detail: "broken", fix: "reinstall" })) },
    ];
    return checks;
  };
  it("starts idle with a status entry per check and no lastRun", () => {
    const { result } = renderHook(() => useDoctor(mk()));
    expect(result.current.status).toEqual({ a: { state: "idle" }, b: { state: "idle" } });
    expect(result.current.summary).toEqual({ passed: 0, total: 2, lastRun: null });
    expect(result.current.running).toBe(false);
  });
  it("runAll runs every check and summarises", async () => {
    const checks = mk();
    const onResult = vi.fn();
    const { result } = renderHook(() => useDoctor(checks, { onResult }));
    await act(async () => { await result.current.runAll(); });
    expect(result.current.status.a).toEqual({ state: "done", result: ok() });
    expect(result.current.status.b).toEqual({ state: "done", result: { ok: false, detail: "broken", fix: "reinstall" } });
    expect(result.current.summary.passed).toBe(1);
    expect(result.current.summary.total).toBe(2);
    expect(typeof result.current.summary.lastRun).toBe("number");
    expect(onResult).toHaveBeenCalledTimes(2);
    expect(result.current.running).toBe(false);
  });
  it("runOne runs only that check; an unknown id resolves null", async () => {
    const checks = mk();
    const { result } = renderHook(() => useDoctor(checks));
    let r: DoctorResult | null = null;
    await act(async () => { r = await result.current.runOne("a"); });
    expect(r).toEqual(ok());
    expect(checks[0]!.run).toHaveBeenCalledTimes(1);
    expect(checks[1]!.run).not.toHaveBeenCalled();
    expect(result.current.status.b).toEqual({ state: "idle" });
    expect(result.current.summary).toMatchObject({ passed: 1, total: 2 });
    await act(async () => { expect(await result.current.runOne("nope")).toBeNull(); });
  });
  it("running is true while a check is in flight", async () => {
    let release!: (r: DoctorResult) => void;
    const checks: DoctorCheck[] = [{ id: "a", label: "A", run: () => new Promise((r) => { release = r; }) }];
    const { result } = renderHook(() => useDoctor(checks));
    let p!: Promise<unknown>;
    act(() => { p = result.current.runAll(); });
    expect(result.current.running).toBe(true);
    expect(result.current.status.a).toEqual({ state: "running" });
    await act(async () => { release(ok()); await p; });
    expect(result.current.running).toBe(false);
  });
  it("a throwing or rejecting check is a failed check carrying the message", async () => {
    const checks: DoctorCheck[] = [
      { id: "t", label: "T", run: () => { throw new Error("sync boom"); } },
      { id: "r", label: "R", run: async () => { throw new Error("async boom"); } },
    ];
    const { result } = renderHook(() => useDoctor(checks));
    await act(async () => { await result.current.runAll(); });
    expect(result.current.status.t).toEqual({ state: "done", result: { ok: false, detail: "sync boom" } });
    expect(result.current.status.r).toEqual({ state: "done", result: { ok: false, detail: "async boom" } });
    expect(result.current.summary.passed).toBe(0);
  });
  it("a stale run never overwrites a newer one", async () => {
    const resolvers: Array<(r: DoctorResult) => void> = [];
    const onResult = vi.fn();
    const checks: DoctorCheck[] = [{ id: "a", label: "A", run: () => new Promise((r) => resolvers.push(r)) }];
    const { result } = renderHook(() => useDoctor(checks, { onResult }));
    let first!: Promise<DoctorResult | null>;
    let second!: Promise<DoctorResult | null>;
    act(() => { first = result.current.runOne("a"); });
    act(() => { second = result.current.runOne("a"); });
    await act(async () => { resolvers[1]!(ok("new")); await second; });
    await act(async () => { resolvers[0]!({ ok: false, detail: "old" }); expect(await first).toBeNull(); });
    expect(result.current.status.a).toEqual({ state: "done", result: ok("new") });
    expect(onResult).toHaveBeenCalledTimes(1);
  });
  it("a stale result does not clear running while the newer run is still in flight", async () => {
    const resolvers: Array<(r: DoctorResult) => void> = [];
    const checks: DoctorCheck[] = [{ id: "a", label: "A", run: () => new Promise((r) => resolvers.push(r)) }];
    const { result } = renderHook(() => useDoctor(checks));
    act(() => { void result.current.runOne("a"); });
    act(() => { void result.current.runOne("a"); });
    await act(async () => { resolvers[0]!(ok("old")); await Promise.resolve(); });
    expect(result.current.status.a).toEqual({ state: "running" });
    await act(async () => { resolvers[1]!(ok("new")); await Promise.resolve(); });
    await waitFor(() => expect(result.current.status.a).toEqual({ state: "done", result: ok("new") }));
  });
  it("ignores results that arrive after unmount", async () => {
    let release!: (r: DoctorResult) => void;
    const onResult = vi.fn();
    const checks: DoctorCheck[] = [{ id: "a", label: "A", run: () => new Promise((r) => { release = r; }) }];
    const { result, unmount } = renderHook(() => useDoctor(checks, { onResult }));
    let p!: Promise<DoctorResult | null>;
    act(() => { p = result.current.runOne("a"); });
    unmount();
    release(ok());
    expect(await p).toBeNull();
    expect(onResult).not.toHaveBeenCalled();
  });
  it("autoRun runs every check once on mount", async () => {
    const checks = mk();
    const { result, rerender } = renderHook(() => useDoctor(checks, { autoRun: true }));
    await waitFor(() => expect(result.current.summary.passed).toBe(1));
    rerender();
    expect(checks[0]!.run).toHaveBeenCalledTimes(1);
  });
});
