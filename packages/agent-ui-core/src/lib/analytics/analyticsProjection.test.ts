import { describe, expect, it, vi } from "vitest";
import {
  ANALYTICS_QUEUE_CAPACITY,
  ANALYTICS_STORAGE_KEY,
  createAnalyticsProjection,
  projectAnalyticsEvent,
  readAnalyticsConsent,
  writeAnalyticsConsent,
  type ProjectedAnalyticsEvent,
} from "./analyticsProjection.js";

const context = { version: "0.1.79", platform: "windows", arch: "x64", build: "debug" as const };

function event(index = 1): ProjectedAnalyticsEvent {
  return projectAnalyticsEvent(
    { name: "project.added", data: {} },
    context,
  );
}

describe("analyticsProjection", () => {
  it("defaults to sharing enabled and persists an explicit opt-out", () => {
    const storage = new StorageShim();
    expect(readAnalyticsConsent(storage)).toBe(true);
    writeAnalyticsConsent(false, storage);
    expect(storage.getItem(ANALYTICS_STORAGE_KEY)).toBe("0");
    expect(readAnalyticsConsent(storage)).toBe(false);
  });

  it("adds only the coarse Waku metadata contract", () => {
    const projected = event();
    expect(projected.name).toBe("project.added");
    expect(projected.data.clientType).toBe("desktop");
    expect(projected.data.version).toBe("0.1.79");
    expect("prompt" in projected.data).toBe(false);
    expect("project_path" in projected.data).toBe(false);
    expect("user_id" in projected.data).toBe(false);
  });

  it("drops queued events when consent is withdrawn", async () => {
    const sent: ProjectedAnalyticsEvent[] = [];
    const projection = createAnalyticsProjection({ track: (value) => { sent.push(value); } }, true);
    projection.setEnabled(false);
    projection.track(event());
    await Promise.resolve();
    expect(sent).toEqual([]);
  });

  it("bounds the queue and continues after a failed send", async () => {
    const sent: ProjectedAnalyticsEvent[] = [];
    const track = vi.fn(async (value: ProjectedAnalyticsEvent) => {
      if (sent.length === 0) throw new Error("offline");
      sent.push(value);
    });
    const projection = createAnalyticsProjection({ track }, true);
    for (let index = 0; index < ANALYTICS_QUEUE_CAPACITY + 4; index += 1) projection.track(event(index));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(track).toHaveBeenCalledTimes(ANALYTICS_QUEUE_CAPACITY);
  });
});

class StorageShim implements Storage {
  private readonly values = new Map<string, string>();
  get length() { return this.values.size; }
  clear() { this.values.clear(); }
  getItem(key: string) { return this.values.get(key) ?? null; }
  key(index: number) { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string) { this.values.delete(key); }
  setItem(key: string, value: string) { this.values.set(key, String(value)); }
}
