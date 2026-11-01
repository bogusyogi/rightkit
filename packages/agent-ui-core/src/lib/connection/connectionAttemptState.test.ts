import assert from "node:assert/strict";
import { test } from "vitest";
import { createConnectionAttemptState } from "./connectionAttemptState.js";

type FakeProcess = { id: string };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

test("an invalidated attempt cannot publish a late descriptor", async () => {
  const state = createConnectionAttemptState<FakeProcess, string>();
  const oldProbe = deferred<string>();
  const oldAttempt = state.startAttempt();
  const oldResult = oldProbe.promise.then((descriptor) => {
    if (!state.isCurrentAttempt(oldAttempt)) {
      throw new Error("App daemon start was superseded by a newer connection attempt.");
    }
    return descriptor;
  });

  assert.equal(state.setPromise(oldAttempt, oldResult), true);
  state.invalidate();
  const newAttempt = state.startAttempt();
  const newResult = Promise.resolve("http://new.example");
  assert.equal(state.setPromise(newAttempt, newResult), true);
  assert.equal(await newResult, "http://new.example");

  oldProbe.resolve("http://old.example");
  await assert.rejects(oldResult, /superseded by a newer connection attempt/);
  assert.equal(state.getPromise(), newResult);
});

test("a stale process exit cannot clear a newer connection attempt", () => {
  const state = createConnectionAttemptState<FakeProcess, string>();
  const oldAttempt = state.startAttempt();
  const oldPromise = Promise.resolve("old");
  assert.equal(state.setPromise(oldAttempt, oldPromise), true);
  const oldOwner = state.attachProcess(oldAttempt, { id: "old" });
  assert.ok(oldOwner);

  state.invalidate();
  const newAttempt = state.startAttempt();
  const newPromise = Promise.resolve("new");
  const newProcess = { id: "new" };
  assert.equal(state.setPromise(newAttempt, newPromise), true);
  assert.ok(state.attachProcess(newAttempt, newProcess));

  assert.equal(state.clearForCurrentProcess(oldOwner), false);
  assert.equal(state.getProcess(), newProcess);
  assert.equal(state.getPromise(), newPromise);
});

test("the current process exit clears its process and connection promise", () => {
  const state = createConnectionAttemptState<FakeProcess, string>();
  const attempt = state.startAttempt();
  const currentPromise = Promise.resolve("current");
  assert.equal(state.setPromise(attempt, currentPromise), true);
  const owner = state.attachProcess(attempt, { id: "current" });
  assert.ok(owner);

  assert.equal(state.clearForCurrentProcess(owner), true);
  assert.equal(state.clearPromiseForAttempt(attempt), true);
  assert.equal(state.getProcess(), null);
  assert.equal(state.getPromise(), null);
});

test("a stale rejected attempt cannot clear a newer connection promise", () => {
  const state = createConnectionAttemptState<FakeProcess, string>();
  const oldAttempt = state.startAttempt();
  assert.equal(state.setPromise(oldAttempt, Promise.resolve("old")), true);
  state.invalidate();

  const newAttempt = state.startAttempt();
  const newPromise = Promise.resolve("new");
  assert.equal(state.setPromise(newAttempt, newPromise), true);

  assert.equal(state.clearPromiseForAttempt(oldAttempt), false);
  assert.equal(state.getPromise(), newPromise);
});

test("an invalidated attempt cannot attach a late-spawned process", () => {
  const state = createConnectionAttemptState<FakeProcess, string>();
  const staleAttempt = state.startAttempt();
  state.invalidate();

  assert.equal(state.attachProcess(staleAttempt, { id: "late" }), null);
  assert.equal(state.getProcess(), null);
});
