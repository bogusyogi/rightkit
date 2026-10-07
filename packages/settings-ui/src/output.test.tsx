// @vitest-environment jsdom
// Pins the built-in licence and doctor markup so refactors onto the headless hooks cannot change default output.
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it } from "vitest";
import type { StoredLicense } from "@rightkit/license";
import { doctorSection, licenseSection } from "./index.js";
import type { DoctorCheck, SettingsContext, SettingsSection } from "./index.js";

afterEach(() => cleanup());

const ctx: SettingsContext = { values: {}, set: async () => {}, patch: async () => {}, saving: false, error: null, clearError: () => {} };
const body = (s: SettingsSection): ReactNode => s.render(ctx);
const active = { status: "active", deviceId: "d", licenseKind: "perpetual", validUntil: null, tier: "pro" } as unknown as StoredLicense;
const client = (lic: StoredLicense | null) => ({ status: async () => lic, activate: async () => active, deactivate: async () => ({}) });
const html = (c: HTMLElement) => c.innerHTML;

describe("licenseSection output", () => {
  it("free, with purchase row", async () => {
    const v = render(<>{body(licenseSection({ client: client(null), purchaseUrl: "https://x.test" }))}</>);
    await screen.findByText("No licence key is active on this device.");
    expect(html(v.container)).toMatchSnapshot();
  });
  it("free, ids, plan slot and custom labels, key typed", async () => {
    const v = render(<>{body(licenseSection({ client: client(null), planSlot: <p>plan</p>, ids: { section: "s", licenseKey: "k", form: "f", activate: "a", deactivate: "d", status: "st" }, labels: { activating: "Working", keyHint: "hint" } }))}</>);
    await userEvent.setup().type(await screen.findByLabelText("Licence key"), "ABC");
    expect(html(v.container)).toMatchSnapshot();
  });
  it("invalid licence", async () => {
    const v = render(<>{body(licenseSection({ client: client({ status: "invalid", message: "Key revoked" } as StoredLicense) }))}</>);
    await screen.findByText("Key revoked");
    expect(html(v.container)).toMatchSnapshot();
  });
  it("active, confirm on (idle then armed)", async () => {
    const v = render(<>{body(licenseSection({ client: client(active), tierLabels: { pro: "Pro" }, ids: { deactivate: "d", status: "st" } }))}</>);
    const btn = await screen.findByRole("button", { name: "Deactivate" });
    expect(html(v.container)).toMatchSnapshot("idle");
    await userEvent.setup().click(btn);
    expect(html(v.container)).toMatchSnapshot("armed");
  });
  it("active with a pro tier shows the mapped tier badge", async () => {
    const pro = { ...active, entitlements: { tier: "pro" } } as unknown as StoredLicense;
    const v = render(<>{body(licenseSection({ client: client(pro), tierLabels: { pro: "Pro Plan" } }))}</>);
    await screen.findByRole("button", { name: "Deactivate" });
    expect(v.container.textContent).toContain("Pro Plan");
  });
  it("active, confirm off", async () => {
    const v = render(<>{body(licenseSection({ client: client(active), confirmDeactivate: false }))}</>);
    await screen.findByRole("button", { name: "Deactivate" });
    expect(html(v.container)).toMatchSnapshot();
  });
  it("activation error", async () => {
    const c = { ...client(null), activate: async () => { throw new Error("offline"); } };
    const v = render(<>{body(licenseSection({ client: c }))}</>);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Licence key"), "K");
    await user.click(screen.getByRole("button", { name: "Activate" }));
    await screen.findByText("offline");
    expect(html(v.container)).toMatchSnapshot();
  });
});

describe("doctorSection output", () => {
  const checks: DoctorCheck[] = [
    { id: "a", label: "A", description: "first", run: async () => ({ ok: true, detail: "fine" }) },
    { id: "b", label: "B", run: async () => ({ ok: false, detail: "broken", fix: "reinstall" }) },
    { id: "c", label: "C", run: () => { throw new Error("boom"); } },
  ];
  it("idle", () => {
    const v = render(<>{body(doctorSection({ checks }))}</>);
    expect(html(v.container)).toMatchSnapshot();
  });
  it("after run all, ids and details wrapper", async () => {
    const v = render(<>{body(doctorSection({ checks, labels: { detailsLabel: "Details", groupTitle: "Engine" }, ids: { section: "sec", summary: "sum", runAll: "all", list: "lst", check: (i) => `c-${i}`, rerun: (i) => `r-${i}` } }))}</>);
    await userEvent.setup().click(screen.getByRole("button", { name: "Run all" }));
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Last run: 1 of 3 passed."));
    expect(html(v.container)).toMatchSnapshot();
  });
  it("running", async () => {
    const v = render(<>{body(doctorSection({ checks: [{ id: "a", label: "A", run: () => new Promise(() => {}) }] }))}</>);
    await userEvent.setup().click(screen.getByRole("button", { name: "Run all" }));
    await screen.findByText("Running diagnostics…");
    expect(html(v.container)).toMatchSnapshot();
  });
  it("empty", () => {
    const v = render(<>{body(doctorSection({ checks: [] }))}</>);
    expect(html(v.container)).toMatchSnapshot();
  });
});
