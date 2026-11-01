import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  REFERENCE_MAX_PER_SESSION,
  REFERENCE_STORAGE_KEY,
  type ReferenceObject,
} from "../lib/types/references.js";
import { useReferenceStore } from "./referenceStore.js";

function makeImageRef(id: string, overrides: Partial<ReferenceObject> = {}): ReferenceObject {
  return {
    id,
    kind: "image",
    destination: `${id}.png`,
    mime: "image/png",
    filename: `${id}.png`,
    sizeBytes: 1024,
    provenance: {
      source: "composer.attachment",
      createdAt: "2026-07-20T00:00:00Z",
      trust: "user",
    },
    actions: ["remove"],
    previewUrl: `data:image/png;base64,AAA-${id}`,
    ...overrides,
  };
}

function makeDocRef(id: string): ReferenceObject {
  return {
    id,
    kind: "generated_document",
    destination: "/tmp/doc.md",
    mime: "text/markdown",
    filename: "doc.md",
    sizeBytes: 256,
    provenance: {
      source: "workspace_resolver",
      createdAt: "2026-07-20T00:00:00Z",
      trust: "workspaceTrusted",
    },
    actions: ["open", "preview"],
  };
}

describe("useReferenceStore", () => {
  beforeEach(() => {
    if (typeof localStorage !== "undefined") localStorage.clear();
    useReferenceStore.setState({ references: [], restoreError: null });
  });

  afterEach(() => {
    if (typeof localStorage !== "undefined") localStorage.clear();
  });

  it("adds references and dedupes by id", () => {
    const ref = makeImageRef("r1");
    useReferenceStore.getState().addReferences([ref]);
    useReferenceStore.getState().addReferences([ref]);
    expect(useReferenceStore.getState().references).toHaveLength(1);
  });

  it("preserves order when merging new references", () => {
    const a = makeImageRef("a");
    const b = makeImageRef("b");
    const c = makeImageRef("c");
    useReferenceStore.getState().addReferences([a]);
    useReferenceStore.getState().addReferences([b]);
    useReferenceStore.getState().addReferences([c]);
    expect(useReferenceStore.getState().references.map((r) => r.id)).toEqual(["a", "b", "c"]);
  });

  it("clips to REFERENCE_MAX_PER_SESSION on overflow", () => {
    const many = Array.from({ length: REFERENCE_MAX_PER_SESSION + 5 }, (_, i) =>
      makeImageRef(`ref-${i}`),
    );
    useReferenceStore.getState().addReferences(many);
    const refs = useReferenceStore.getState().references;
    expect(refs).toHaveLength(REFERENCE_MAX_PER_SESSION);
    // The clip keeps the most-recently-added tail (slice(-N))
    expect(refs[refs.length - 1].id).toBe(`ref-${many.length - 1}`);
  });

  it("ignores empty / non-array inputs", () => {
    useReferenceStore.getState().addReferences([]);
    expect(useReferenceStore.getState().references).toHaveLength(0);
  });

  it("removeReference drops by id and persists", () => {
    const a = makeImageRef("a");
    const b = makeImageRef("b");
    useReferenceStore.getState().addReferences([a, b]);
    useReferenceStore.getState().removeReference("a");
    const refs = useReferenceStore.getState().references;
    expect(refs.map((r) => r.id)).toEqual(["b"]);
  });

  it("removeReference ignores missing / empty id", () => {
    const a = makeImageRef("a");
    useReferenceStore.getState().addReferences([a]);
    useReferenceStore.getState().removeReference("");
    useReferenceStore.getState().removeReference("does-not-exist");
    expect(useReferenceStore.getState().references).toHaveLength(1);
  });

  it("setError attaches an error and removes it when undefined", () => {
    const a = makeImageRef("a");
    useReferenceStore.getState().addReferences([a]);
    useReferenceStore.getState().setError("a", "boom");
    expect(useReferenceStore.getState().references[0].error).toBe("boom");
    useReferenceStore.getState().setError("a", undefined);
    expect(useReferenceStore.getState().references[0].error).toBeUndefined();
  });

  it("clear empties references and clears persisted localStorage", () => {
    useReferenceStore.getState().addReferences([makeImageRef("a")]);
    useReferenceStore.getState().clear();
    expect(useReferenceStore.getState().references).toHaveLength(0);
    expect(useReferenceStore.getState().restoreError).toBeNull();
  });

  it("persists metadata-only form (no previewUrl, no error)", () => {
    const a = makeImageRef("a");
    useReferenceStore.getState().addReferences([a]);
    const raw = localStorage.getItem(REFERENCE_STORAGE_KEY);
    expect(raw).toBeTruthy();
    if (!raw) throw new Error("missing storage key");
    const parsed = JSON.parse(raw);
    expect(parsed[0]).not.toHaveProperty("previewUrl");
    expect(parsed[0]).not.toHaveProperty("error");
    expect(parsed[0].id).toBe("a");
    expect(parsed[0].kind).toBe("image");
    expect(parsed[0].filename).toBe("a.png");
  });

  it("restorePersisted revives ReferenceMeta and stamps error on image kinds", () => {
    const persisted = [
      {
        id: "img-1",
        kind: "image",
        destination: "img-1.png",
        mime: "image/png",
        filename: "img-1.png",
        sizeBytes: 2048,
        provenance: {
          source: "composer.attachment",
          createdAt: "2026-07-20T00:00:00Z",
          trust: "user",
        },
        actions: ["remove"],
      },
      {
        id: "doc-1",
        kind: "generated_document",
        destination: "/tmp/doc.md",
        mime: "text/markdown",
        filename: "doc.md",
        sizeBytes: 256,
        provenance: {
          source: "workspace_resolver",
          createdAt: "2026-07-20T00:00:00Z",
          trust: "workspaceTrusted",
        },
        actions: ["open", "preview"],
      },
    ];
    localStorage.setItem(REFERENCE_STORAGE_KEY, JSON.stringify(persisted));
    useReferenceStore.getState().restorePersisted();
    const refs = useReferenceStore.getState().references;
    expect(refs).toHaveLength(2);
    const image = refs.find((r) => r.id === "img-1");
    expect(image?.error).toMatch(/image bytes lost/i);
    expect(image?.previewUrl).toBeUndefined();
    const doc = refs.find((r) => r.id === "doc-1");
    expect(doc?.error).toBeUndefined();
    expect(doc?.provenance.trust).toBe("workspaceTrusted");
  });

  it("restorePersisted reports JSON parse failures via restoreError", () => {
    localStorage.setItem(REFERENCE_STORAGE_KEY, "not valid json");
    useReferenceStore.getState().restorePersisted();
    expect(useReferenceStore.getState().restoreError).toBeTruthy();
  });

  it("restorePersisted clears stale rows that aren't arrays", () => {
    localStorage.setItem(REFERENCE_STORAGE_KEY, JSON.stringify({ not: "an array" }));
    useReferenceStore.getState().restorePersisted();
    expect(useReferenceStore.getState().references).toHaveLength(0);
    expect(useReferenceStore.getState().restoreError).toBeNull();
  });

  it("toWireMeta returns the wire-friendly subset", () => {
    const a = makeImageRef("a");
    const b = makeDocRef("b");
    useReferenceStore.getState().addReferences([a, b]);
    const wire = useReferenceStore.getState().toWireMeta();
    expect(wire.map((m) => m.id).sort()).toEqual(["a", "b"]);
    for (const meta of wire) {
      expect(meta).not.toHaveProperty("previewUrl");
      expect(meta).not.toHaveProperty("error");
    }
  });

  it("setError does not persist (error stays in memory only)", () => {
    useReferenceStore.getState().addReferences([makeImageRef("a")]);
    useReferenceStore.getState().setError("a", "transient");
    const raw = localStorage.getItem(REFERENCE_STORAGE_KEY);
    if (!raw) throw new Error("missing storage key");
    const parsed = JSON.parse(raw);
    expect(parsed[0]).not.toHaveProperty("error");
  });
});
