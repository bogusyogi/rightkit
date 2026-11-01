import assert from "node:assert/strict";
import { test } from "node:test";
import { loadTranscriptDetail, type InvokeFn } from "../../src/queue/lib/transcriptLoad";

function fakeInvoke(responses: Record<string, unknown | Error>, calls: string[]): InvokeFn {
  return (async (cmd: string, args?: Record<string, unknown>) => {
    calls.push(`${cmd}:${JSON.stringify(args)}`);
    const r = responses[cmd];
    if (r instanceof Error) throw r;
    return r;
  }) as InvokeFn;
}

test("loadTranscriptDetail merges the full text variant over the slim preview", async () => {
  const calls: string[] = [];
  const invokeFn = fakeInvoke(
    {
      get_transcript: { id: "j1", text: "preview" },
      get_transcript_variant: { text: "full body" },
    },
    calls,
  );
  const out = await loadTranscriptDetail(invokeFn, "j1");
  assert.equal(out.text, "full body");
  assert.equal(out.id, "j1");
  assert.deepEqual(calls, [
    'get_transcript:{"id":"j1"}',
    'get_transcript_variant:{"id":"j1","kind":"text"}',
  ]);
});

test("loadTranscriptDetail keeps the preview when the variant fetch fails", async () => {
  const invokeFn = fakeInvoke(
    {
      get_transcript: { id: "j2", text: "preview" },
      get_transcript_variant: new Error("boom"),
    },
    [],
  );
  const out = await loadTranscriptDetail(invokeFn, "j2");
  assert.equal(out.text, "preview");
});

test("loadTranscriptDetail rejects when the slim fetch fails", async () => {
  const invokeFn = fakeInvoke({ get_transcript: new Error("no row") }, []);
  await assert.rejects(loadTranscriptDetail(invokeFn, "j3"), /no row/);
});
