import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { FuseCurves } from "./layout.js";

describe("FuseCurves", () => {
  it("keeps default vertical markup and positioning", () => {
    expect(renderToStaticMarkup(<FuseCurves />)).toBe(
      '<svg class="rk-fuse rk-fuse--top" data-rk-fuse="top" aria-hidden="true" focusable="false" width="12" height="12" viewBox="0 0 12 12" style="position:absolute;right:0;top:-12px;width:12px;height:12px;pointer-events:none"><path d="M12 0V12H0A12 12 0 0 0 12 0Z"></path></svg><svg class="rk-fuse rk-fuse--bottom" data-rk-fuse="bottom" aria-hidden="true" focusable="false" width="12" height="12" viewBox="0 0 12 12" style="position:absolute;right:0;bottom:-12px;width:12px;height:12px;pointer-events:none;transform:scaleY(-1)"><path d="M12 0V12H0A12 12 0 0 0 12 0Z"></path></svg>',
    );
  });
});
