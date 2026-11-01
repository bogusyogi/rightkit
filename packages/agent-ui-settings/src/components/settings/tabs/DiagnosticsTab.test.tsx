import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import DiagnosticsTab from "./DiagnosticsTab.js";
import type { DiagnosticsReport, SupportBundle } from "@rightkit/agent-ui-core/lib/types";

function baseSupportBundle(overrides: Partial<SupportBundle> = {}): SupportBundle {
  return {
    bundleId: "bundle-1",
    fileName: "bundle-1.zip",
    byteLength: 2048,
    crashReports: 2,
    nativeArtifacts: 0,
    logEvents: 7,
    downloadUrl: "/diagnostics/bundles/bundle-1",
    ...overrides,
  };
}

const noop = () => {};

describe("DiagnosticsTab support bundle", () => {
  it("shows 'no bundle created' when nothing has been generated this session", () => {
    const html = renderToStaticMarkup(
      <DiagnosticsTab
        config={null}
        report={null}
        supportBundle={null}
        supportBundlePending={null}
        instructionStatus={null}
        loading={false}
        error={null}
        onRefresh={noop}
        onCreateBundle={noop}
        onDownloadBundle={noop}
      />,
    );

    expect(html).toContain("Support bundle");
    expect(html).toContain("No bundle created this session");
    expect(html).toContain("Create bundle");
  });

  it("shows the ZIP summary counts and privacy line once created", () => {
    const html = renderToStaticMarkup(
      <DiagnosticsTab
        config={null}
        report={null}
        supportBundle={baseSupportBundle()}
        supportBundlePending={null}
        instructionStatus={null}
        loading={false}
        error={null}
        onRefresh={noop}
        onCreateBundle={noop}
        onDownloadBundle={noop}
      />,
    );

    expect(html).toContain("bundle-1");
    expect(html).toContain("2048 bytes");
    expect(html).toContain("Crash reports");
    expect(html).toContain("Nothing is uploaded.");
  });

  it("shows the Creating label and disables both buttons while a bundle is being created", () => {
    const html = renderToStaticMarkup(
      <DiagnosticsTab
        config={null}
        report={null}
        supportBundle={null}
        supportBundlePending="create"
        instructionStatus={null}
        loading={false}
        error={null}
        onRefresh={noop}
        onCreateBundle={noop}
        onDownloadBundle={noop}
      />,
    );

    expect(html).toContain("Creating");
    expect((html.match(/disabled=""/g) ?? []).length).toBe(2);
  });
});

describe("DiagnosticsTab check status tone (PA-06a)", () => {
  it("renders a warn check in amber, not the command-accent red", () => {
    const report: DiagnosticsReport = {
      verdict: "warn",
      checks: [
        {
          id: "sandbox",
          label: "Sandbox tier",
          status: "warn",
          detail: "Landlock unavailable on this host.",
          fix: "Install landlock or accept the reduced tier.",
        },
      ],
    };
    const html = renderToStaticMarkup(
      <DiagnosticsTab
        config={null}
        report={report}
        supportBundle={null}
        supportBundlePending={null}
        instructionStatus={null}
        loading={false}
        error={null}
        onRefresh={noop}
        onCreateBundle={noop}
        onDownloadBundle={noop}
      />,
    );

    expect(html).toContain("Warn");
    expect(html).toContain("border-warning/40 text-warning");
    expect(html).not.toContain("border-accent/40 text-accent");
  });
});
