import { describe, expect, it } from "vitest";

import {
  decodeSessionShareCode,
  encodeSessionShareCode,
  SessionShareCodeError,
  type SessionShareGraph,
} from "./sessionShareCode";

function sampleGraph(): SessionShareGraph {
  return {
    clusters: [],
    edges: [
      { source: "s-a", target: "s-b" },
      { source: "s-b", target: "s-c" },
    ],
    nodes: [
      {
        category: "workspace",
        id: "s-a",
        label: "API hardening",
        model: "z-ai/glm-5.2",
        status: "running",
        timestamp: 1_700_000_000,
      },
      {
        category: "workspace",
        id: "s-b",
        label: "Review daemon changes",
        model: "z-ai/glm-5.2",
        status: "unread",
        timestamp: 1_700_010_000,
      },
      {
        category: "workspace",
        id: "s-c",
        label: "Release notes",
        model: null,
        status: "idle",
        timestamp: null,
      },
    ],
    stats: {},
  };
}

const topology = (graph: SessionShareGraph): [number, number][] => {
  const indexes = new Map(graph.nodes.map((node, index) => [node.id, index]));
  return graph.edges.map((edge) => [indexes.get(edge.source)!, indexes.get(edge.target)!]);
};

describe("sessionShareCode", () => {
  it("preserves the sanitized view graph and topology", () => {
    const original = sampleGraph();
    const decoded = decodeSessionShareCode(encodeSessionShareCode(original));

    expect(decoded.nodes).toHaveLength(original.nodes.length);
    decoded.nodes.forEach((node, index) => {
      const source = original.nodes[index];
      expect(node.label).toBe(source.label);
      expect(node.category).toBe(source.category);
      expect(node.model).toBe(source.model);
      expect(node.status).toBe(source.status);
      if (typeof source.timestamp === "number") {
        expect(Math.abs((node.timestamp ?? 0) - source.timestamp)).toBeLessThanOrEqual(4);
      } else {
        expect(node.timestamp).toBeNull();
      }
    });
    expect(topology(decoded)).toEqual(topology(original));
  });

  it("is opaque, URL-safe, versioned, and excludes transcript fields", () => {
    const code = encodeSessionShareCode(sampleGraph());
    expect(code.startsWith("CRS")).toBe(true);
    expect(code.slice(3)).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(code.length).toBeLessThan(JSON.stringify(sampleGraph()).length);
    expect(code).not.toContain("API hardening");
    expect(code).not.toContain("prompt");
  });

  it("drops edges whose endpoints are absent", () => {
    const graph = sampleGraph();
    graph.edges.push({ source: "s-a", target: "missing" });
    expect(decodeSessionShareCode(encodeSessionShareCode(graph)).edges).toHaveLength(2);
  });

  it("handles empty graphs and wrapped clipboard input", () => {
    const empty: SessionShareGraph = { clusters: [], edges: [], nodes: [], stats: {} };
    const code = encodeSessionShareCode(empty);
    expect(decodeSessionShareCode(` ${code.slice(0, 10)}\n${code.slice(10)}\t`).nodes).toHaveLength(0);
  });

  it("rejects garbage and checksum corruption", () => {
    expect(() => decodeSessionShareCode("not a real code !!!")).toThrow(SessionShareCodeError);
    const code = encodeSessionShareCode(sampleGraph());
    const index = Math.floor(code.length / 2);
    const corrupted = `${code.slice(0, index)}${code[index] === "A" ? "B" : "A"}${code.slice(index + 1)}`;
    expect(() => decodeSessionShareCode(corrupted)).toThrow(SessionShareCodeError);
  });
});
