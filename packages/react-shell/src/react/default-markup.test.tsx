// Pins the built-in Titlebar markup so token and prop additions cannot change default output.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { WindowControlAdapter } from "@rightkit/platform-ui";
import { Titlebar } from "./layout.js";

const adapter: WindowControlAdapter = { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {} };

describe("Titlebar default markup", () => {
  it("windows: start, title, end and caption buttons", () => {
    expect(
      renderToStaticMarkup(<Titlebar platform="windows" windowControls={adapter} start={<span>S</span>} title="Doc" end={<span>E</span>} />),
    ).toMatchSnapshot();
  });
  it("macos: traffic-light space and no caption buttons", () => {
    expect(renderToStaticMarkup(<Titlebar platform="macos" title="Doc" start={<span>S</span>} />)).toMatchSnapshot();
  });
  it("minimal: no start, title, end or adapter", () => {
    expect(renderToStaticMarkup(<Titlebar platform="linux" />)).toMatchSnapshot();
  });
});
