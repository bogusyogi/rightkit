import assert from "node:assert/strict";
import { test } from "node:test";
import { createFlashController, type FlashTimers } from "../../src/queue/lib/flash";
import {
  INITIAL_TRANSCRIPT_STATE,
  transcriptReducer,
  type TranscriptState,
} from "../../src/queue/lib/transcriptHooks";
import { runTranscriptFileAction, TRANSCRIPT_FILE_COMMANDS } from "../../src/queue/lib/transcriptActions";
import type { InvokeFn } from "../../src/queue/lib/transcriptLoad";

function fakeTimers() {
  let next = 1;
  const pending = new Map<number, () => void>();
  const timers: FlashTimers = {
    setTimeout(fn) {
      const id = next++;
      pending.set(id, fn);
      return id;
    },
    clearTimeout(handle) {
      pending.delete(handle as number);
    },
  };
  return { timers, pending, fire: () => {
      const due = [...pending.entries()];
      pending.clear();
      for (const [, f] of due) f();
    } };
}

test("flash turns on, turns off when its timer fires, and a re-trigger keeps one timer", () => {
  const t = fakeTimers();
  const states: boolean[] = [];
  const flash = createFlashController(1500, (on) => states.push(on), t.timers);
  flash.trigger();
  flash.trigger();
  assert.equal(t.pending.size, 1, "re-trigger must replace the pending timer");
  t.fire();
  assert.deepEqual(states, [true, true, false]);
  assert.equal(t.pending.size, 0);
});

test("flash reset and dispose cancel the pending timer without a late state write", () => {
  const t = fakeTimers();
  const states: boolean[] = [];
  const flash = createFlashController(1500, (on) => states.push(on), t.timers);
  flash.trigger();
  flash.dispose();
  assert.equal(t.pending.size, 0);
  flash.trigger();
  flash.reset();
  assert.equal(t.pending.size, 0);
  assert.deepEqual(states, [true, true, false]);
});

const detail = { id: "j1", text: "hello" } as unknown as TranscriptState["detail"];

test("transcript reducer: start clears the error, loaded stores the detail", () => {
  let s = transcriptReducer(INITIAL_TRANSCRIPT_STATE, { type: "start" });
  assert.equal(s.loading, true);
  s = transcriptReducer(s, { type: "loaded", detail: detail! });
  assert.equal(s.loading, false);
  assert.equal(s.detail, detail);
  assert.equal(s.error, null);
});

test("transcript reducer: failure drops the detail and keeps the message", () => {
  const s = transcriptReducer(
    { detail, loading: true, error: null },
    { type: "failed", message: "no row" },
  );
  assert.deepEqual(s, { detail: null, loading: false, error: "no row" });
});

test("transcript reducer: reset clears everything", () => {
  const s = transcriptReducer(
    { detail, loading: false, error: "x" },
    { type: "reset" },
  );
  assert.deepEqual(s, INITIAL_TRANSCRIPT_STATE);
});

test("each file action maps to its Tauri command with the transcript id", async () => {
  const calls: Array<[string, unknown]> = [];
  const invokeFn = (async (cmd: string, args?: Record<string, unknown>) => {
    calls.push([cmd, args]);
  }) as InvokeFn;
  const errors: string[] = [];
  for (const action of Object.keys(TRANSCRIPT_FILE_COMMANDS) as Array<keyof typeof TRANSCRIPT_FILE_COMMANDS>) {
    runTranscriptFileAction(invokeFn, action, "j9", (m) => errors.push(m));
  }
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(calls, [
    ["open_source_url", { id: "j9" }],
    ["open_file", { id: "j9" }],
    ["reveal_in_folder", { id: "j9" }],
    ["open_media_file", { id: "j9" }],
    ["reveal_media_in_folder", { id: "j9" }],
  ]);
  assert.deepEqual(errors, []);
});

test("a failed file action reports the formatted error", async () => {
  const invokeFn = (async () => {
    throw "disk gone";
  }) as InvokeFn;
  const errors: string[] = [];
  runTranscriptFileAction(invokeFn, "openMedia", "j1", (m) => errors.push(m));
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(errors, ["disk gone"]);
});
