import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { RemoteStatusCard } from "./RemoteStatusCard";

describe("execution RemoteStatusCard warn tone (PA-06a)", () => {
  it("renders a warn status in amber, not the command-accent red", () => {
    const html = renderToStaticMarkup(
      <RemoteStatusCard title="Network profiles" value="0 profiles" status="warn" />,
    );
    expect(html).toMatch(/border-warning\/40/);
    expect(html).toMatch(/bg-warning\/10/);
    expect(html).not.toMatch(/border-accent\/40/);
    expect(html).not.toMatch(/bg-accent\/10/);
  });

  it("still renders pass in success green and block in danger red", () => {
    const pass = renderToStaticMarkup(<RemoteStatusCard title="t" value="v" status="pass" />);
    expect(pass).toMatch(/accent-2/);

    const block = renderToStaticMarkup(<RemoteStatusCard title="t" value="v" status="block" />);
    expect(block).toMatch(/border-danger\/40/);
  });
});
