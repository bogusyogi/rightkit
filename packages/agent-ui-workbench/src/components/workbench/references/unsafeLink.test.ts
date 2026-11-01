import { describe, expect, it } from "vitest";

// The unsafe-scheme regex mirrors the one in ReferenceLink.tsx. This file
// documents the trust boundary: the desktop composer must NEVER render a
// clickable element for these schemes — even when the rest of the renderer
// would otherwise present an <a> tag.

const UNSAFE_SCHEMES = /^(javascript|data|vbscript|file):/i;

describe("ReferenceLink unsafe-scheme blocklist", () => {
  it("blocks javascript:", () => {
    expect(UNSAFE_SCHEMES.test("javascript:alert(1)")).toBe(true);
    expect(UNSAFE_SCHEMES.test("JavaScript:alert(1)")).toBe(true);
  });

  it("blocks vbscript:", () => {
    expect(UNSAFE_SCHEMES.test("vbscript:msgbox(1)")).toBe(true);
  });

  it("blocks file: URLs", () => {
    expect(UNSAFE_SCHEMES.test("file:///etc/passwd")).toBe(true);
    expect(UNSAFE_SCHEMES.test("FILE:///etc/passwd")).toBe(true);
  });

  it("blocks bare data: URLs (data:image/png;...) — only image data: URLs are allowed via ReferenceMeta.mime", () => {
    // ReferenceLink itself blocks ALL data: URLs; image bytes travel via
    // previewUrl, not destination. This keeps the renderer safe even if a
    // sender tricks a `data:text/html;base64,...` payload through the wire.
    expect(UNSAFE_SCHEMES.test("data:text/html,<script>alert(1)</script>")).toBe(true);
    expect(UNSAFE_SCHEMES.test("data:image/png;base64,iVBORw0K")).toBe(true);
  });

  it("does NOT block https or http URLs", () => {
    expect(UNSAFE_SCHEMES.test("https://example.com")).toBe(false);
    expect(UNSAFE_SCHEMES.test("http://example.com/page")).toBe(false);
  });

  it("does NOT block workspace paths or mailto:", () => {
    expect(UNSAFE_SCHEMES.test("/tmp/foo.txt")).toBe(false);
    expect(UNSAFE_SCHEMES.test("mailto:foo@example.com")).toBe(false);
    expect(UNSAFE_SCHEMES.test("D:\\path\\to\\file")).toBe(false);
  });
});
