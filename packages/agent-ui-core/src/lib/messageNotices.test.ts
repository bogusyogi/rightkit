import { describe, expect, it } from "vitest";
import {
  formatBytes,
  parseBracketedNotice,
  parseExternalizedOutput,
  stripAgentBookkeeping,
} from "./messageNotices.js";

describe("parseExternalizedOutput", () => {
  const stub = [
    "[Large tool output externalized]",
    "Full output: .coderight/tool-results/8d4da951.txt",
    "Reference: ref:8d4da951691394423d7e3b1a02314744fea26326273610bab65720331d9308c4",
    "Bytes: 12667",
    "",
    "--- head ---",
    "    1:204: # DISPATCH",
  ].join("\n");

  it("extracts the parts a human needs and drops the model-facing header", () => {
    const parsed = parseExternalizedOutput(stub);
    expect(parsed?.bytes).toBe(12667);
    expect(parsed?.preview.startsWith("--- head ---")).toBe(true);
    // The ref hash and internal path must not survive into the preview body.
    expect(parsed?.preview).not.toContain("8d4da951691394423d7e3b1a02314744");
  });

  it("leaves ordinary tool output alone", () => {
    expect(parseExternalizedOutput("total 24\ndrwxr-xr-x  5 adrian")).toBeNull();
  });
});

describe("formatBytes", () => {
  it("reads as a size, not a raw count", () => {
    expect(formatBytes(512)).toBe("512 B");
    // A decimal only earns its place below 10 KB; past that it is noise.
    expect(formatBytes(4096)).toBe("4.0 KB");
    expect(formatBytes(12667)).toBe("12 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB");
  });
});

describe("parseBracketedNotice", () => {
  it("treats a whole-body gate notice as status", () => {
    const notice = parseBracketedNotice(
      "[Loop-safety gate: required diagnostic infrastructure could not verify 1 edited file(s); failing closed.]",
    );
    expect(notice).toContain("Loop-safety gate");
    // The wrapping brackets are chrome, not content.
    expect(notice?.startsWith("[")).toBe(false);
  });

  it("does not restyle prose that merely contains brackets", () => {
    expect(parseBracketedNotice("I edited [the gate] and it now fails closed.")).toBeNull();
    // A bracketed word that is not a notice stays prose.
    expect(parseBracketedNotice("[TODO]")).toBeNull();
  });
});

describe("stripAgentBookkeeping", () => {
  it("removes the todo checklist, which the todo dock renders as live state", () => {
    const out = stripAgentBookkeeping(
      ["Todos (0/2 done):", "[~] Verifying bridge", "[ ] Wave 1", "real output line"].join("\n"),
    );
    expect(out).toBe("real output line");
  });

  it("removes EXIT sentinels but keeps the command's real output", () => {
    const out = stripAgentBookkeeping(
      ["ssh: connect to host 192.168.1.3 port 22: Operation not permitted", "---EXIT 0---"].join(
        "\n",
      ),
    );
    expect(out).toBe("ssh: connect to host 192.168.1.3 port 22: Operation not permitted");
  });

  it("never edits inside a line", () => {
    const line = "echo '---EXIT 0---' is how the harness marks the end";
    expect(stripAgentBookkeeping(line)).toBe(line);
  });
});
