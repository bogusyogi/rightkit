// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { useSettings } from "./useSettings.js";
import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "./types.js";

afterEach(cleanup);

/** A backend whose first load() is held open until `releaseLoad` is called. */
function slowLoadBackend(stored: SettingsValues, failPatch = false) {
  let revision = 1;
  let releaseLoad!: () => void;
  let first = true;
  let loads = 0;
  const snapshot = (): SettingsSnapshot => ({ values: { ...stored }, revision });
  const backend: SettingsBackend = {
    load: () => {
      loads += 1;
      if (!first) return Promise.resolve(snapshot());
      first = false;
      const before = snapshot();
      return new Promise((resolve) => (releaseLoad = () => resolve(before)));
    },
    patch: async (p) => {
      if (failPatch) throw new Error("disk full");
      for (const [k, v] of Object.entries(p)) {
        if (v === null) delete stored[k];
        else stored[k] = v;
      }
      revision += 1;
      return snapshot();
    },
  };
  return { backend, releaseLoad: () => releaseLoad(), loads: () => loads };
}

describe("useSettings reload against pending writes", () => {
  it("a load that finishes while a write is pending does not overwrite the written value", async () => {
    const { backend, releaseLoad } = slowLoadBackend({ theme: "light", size: 1 });
    const { result } = renderHook(() => useSettings(backend, { initial: { values: { theme: "light" }, revision: 1 } }));
    let write!: Promise<void>;
    act(() => {
      write = result.current.set("theme", "dark");
    });
    expect(result.current.values.theme).toBe("dark");
    await act(async () => {
      releaseLoad(); // the mount-time load answers with the state from before the write
      await write;
    });
    await waitFor(() => expect(result.current.saving).toBe(false));
    expect(result.current.values).toEqual({ theme: "dark", size: 1 });
  });

  it("a skipped load is repeated after a failed write, so the stored values still arrive", async () => {
    const { backend, releaseLoad, loads } = slowLoadBackend({ theme: "light", size: 1 }, true);
    const { result } = renderHook(() => useSettings(backend, { initial: { values: { theme: "light" }, revision: 1 } }));
    let write!: Promise<void>;
    act(() => {
      write = result.current.set("theme", "dark");
    });
    await act(async () => {
      releaseLoad();
      await write;
    });
    await waitFor(() => expect(result.current.values).toEqual({ theme: "light", size: 1 }));
    expect(result.current.error).toBe("disk full");
    expect(loads()).toBe(2);
  });
});

/** A promise the test settles by hand. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** An immediate backend recording patch revisions. */
function immediateBackend(values: SettingsValues, revision: number) {
  const patchRevisions: (number | undefined)[] = [];
  let rev = revision;
  const backend: SettingsBackend = {
    load: () => Promise.resolve({ values: { ...values }, revision: rev }),
    patch: (p, expected) => {
      patchRevisions.push(expected);
      for (const [k, v] of Object.entries(p)) {
        if (v === null) delete values[k];
        else values[k] = v;
      }
      rev += 1;
      return Promise.resolve({ values: { ...values }, revision: rev });
    },
  };
  return { backend, patchRevisions };
}

describe("useSettings backend switch (generation ownership)", () => {
  it("a delayed load and load-error from backend A never change backend B, and B patches use B's revision", async () => {
    const aLoad = deferred<SettingsSnapshot>();
    const a: SettingsBackend = { load: () => aLoad.promise, patch: () => Promise.reject(new Error("unused")) };
    const b = immediateBackend({ theme: "b" }, 7);
    const { result, rerender } = renderHook(({ bk }: { bk: SettingsBackend }) => useSettings(bk), { initialProps: { bk: a } });
    rerender({ bk: b.backend });
    await waitFor(() => expect(result.current.values).toEqual({ theme: "b" }));
    await act(async () => {
      aLoad.reject(new Error("a is gone"));
      await aLoad.promise.catch(() => {});
    });
    expect(result.current.values).toEqual({ theme: "b" });
    expect(result.current.error).toBeNull();
    await act(async () => {
      await result.current.set("theme", "b2");
    });
    expect(b.patchRevisions).toEqual([7]);
    expect(result.current.values).toEqual({ theme: "b2" });
  });

  it("a delayed A write settles without mutating B, and a queued stale A write never starts", async () => {
    const aPatch = deferred<SettingsSnapshot>();
    let aPatchCalls = 0;
    const a: SettingsBackend = {
      load: () => Promise.resolve({ values: { theme: "a" }, revision: 1 }),
      patch: () => {
        aPatchCalls += 1;
        return aPatch.promise;
      },
    };
    const b = immediateBackend({ theme: "b" }, 3);
    const { result, rerender } = renderHook(({ bk }: { bk: SettingsBackend }) => useSettings(bk), { initialProps: { bk: a } });
    await waitFor(() => expect(result.current.values).toEqual({ theme: "a" }));
    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = result.current.set("theme", "a1");
      second = result.current.set("theme", "a2");
    });
    await act(async () => {}); // the first write starts; the second stays queued behind it
    expect(aPatchCalls).toBe(1);
    rerender({ bk: b.backend });
    await waitFor(() => expect(result.current.values).toEqual({ theme: "b" }));
    await act(async () => {
      aPatch.resolve({ values: { theme: "a1" }, revision: 2 });
      await first;
      await second;
    });
    expect(aPatchCalls).toBe(1);
    expect(result.current.values).toEqual({ theme: "b" });
    expect(result.current.saving).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("a stale conflict does not retry after a switch and a stale failure does not roll back B", async () => {
    const aPatch = deferred<SettingsSnapshot>();
    let aLoads = 0;
    let aPatchCalls = 0;
    const a: SettingsBackend = {
      load: () => {
        aLoads += 1;
        return Promise.resolve({ values: { theme: "a" }, revision: 1 });
      },
      patch: () => {
        aPatchCalls += 1;
        return aPatch.promise;
      },
    };
    const b = immediateBackend({ theme: "b" }, 5);
    const { result, rerender } = renderHook(({ bk }: { bk: SettingsBackend }) => useSettings(bk), { initialProps: { bk: a } });
    await waitFor(() => expect(result.current.values).toEqual({ theme: "a" }));
    let write!: Promise<void>;
    act(() => {
      write = result.current.set("theme", "a1");
    });
    await act(async () => {}); // the write starts against A before the switch
    expect(aPatchCalls).toBe(1);
    rerender({ bk: b.backend });
    await waitFor(() => expect(result.current.values).toEqual({ theme: "b" }));
    const loadsBefore = aLoads;
    await act(async () => {
      aPatch.reject(Object.assign(new Error("stale"), { code: "revision_conflict" }));
      await write;
    });
    expect(aPatchCalls).toBe(1);
    expect(aLoads).toBe(loadsBefore);
    expect(result.current.values).toEqual({ theme: "b" });
    expect(result.current.error).toBeNull();
  });
});
