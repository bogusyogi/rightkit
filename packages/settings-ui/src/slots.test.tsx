// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AutoUpdateClient, AutoUpdateStatus } from "@rightkit/app-update";
import type { StoredLicense } from "@rightkit/license";
import { SettingsPanel, UpdatesControls, aboutSection, createMemorySettings, doctorSection, licenseSection, standardSections, updatesSection, useUpdates } from "./index.js";
import type { DoctorCheck, LicenseClientLike, SettingsContext, SettingsSection, UpdatesAdapter } from "./index.js";

afterEach(() => cleanup());

const ctx: SettingsContext = { values: {}, set: async () => {}, patch: async () => {}, saving: false, error: null, clearError: () => {} };
const body = (s: SettingsSection): ReactNode => s.render(ctx);
const sections: SettingsSection[] = [
  { id: "a", label: "Alpha", render: () => <p>alpha body</p> },
  { id: "b", label: "Beta", render: () => <p>beta body</p> },
];
const panel = (props: Partial<Parameters<typeof SettingsPanel>[0]> = {}) => render(<SettingsPanel backend={createMemorySettings()} sections={sections} {...props} />);

describe("SettingsPanel nav slots", () => {
  it("renders navFooter at the bottom of the nav, after the items", async () => {
    panel({ navFooter: <span>Version 1.2.3</span> });
    const nav = await screen.findByRole("navigation");
    const footer = within(nav).getByText("Version 1.2.3");
    expect(footer.parentElement).toHaveClass("rk-settings__navfooter");
    expect(nav.lastElementChild).toBe(footer.parentElement);
  });
  it("omits navFooter when not given", async () => {
    panel();
    await screen.findByRole("navigation");
    expect(document.querySelector(".rk-settings__navfooter")).toBeNull();
  });
  it("heading: default Settings, custom node, and false omits it", async () => {
    panel();
    expect(await screen.findByRole("heading", { level: 2, name: "Settings" })).toBeInTheDocument();
    cleanup();
    panel({ navHeading: <em>Preferences</em> });
    expect(await screen.findByRole("heading", { level: 2, name: "Preferences" })).toBeInTheDocument();
    cleanup();
    panel({ title: "Old title", navHeading: false });
    await screen.findByRole("navigation");
    expect(document.querySelector(".rk-settings__title")).toBeNull();
    expect(screen.queryByText("Old title")).toBeNull();
    cleanup();
    panel({ title: "Old title" });
    expect(await screen.findByRole("heading", { level: 2, name: "Old title" })).toBeInTheDocument();
  });
  it("exposes aria-current and data-active on the active item only", async () => {
    const user = userEvent.setup();
    panel();
    const alpha = await screen.findByRole("button", { name: "Alpha" });
    const beta = screen.getByRole("button", { name: "Beta" });
    expect(alpha).toHaveAttribute("aria-current", "page");
    expect(alpha).toHaveAttribute("data-active", "true");
    expect(beta).not.toHaveAttribute("aria-current");
    expect(beta).not.toHaveAttribute("data-active");
    await user.click(beta);
    expect(beta).toHaveAttribute("data-active", "true");
    expect(alpha).not.toHaveAttribute("data-active");
    expect(screen.getByText("beta body")).toBeInTheDocument();
  });
  it("renderNavItem draws its own item content with the active state", async () => {
    const user = userEvent.setup();
    const renderNavItem = vi.fn((s: SettingsSection, st: { active: boolean }) => <span data-testid={`item-${s.id}`}>{st.active ? ">" : ""}{s.label}</span>);
    panel({ renderNavItem });
    await screen.findByTestId("item-a");
    expect(screen.getByTestId("item-a")).toHaveTextContent(">Alpha");
    expect(screen.getByTestId("item-b")).toHaveTextContent("Beta");
    expect(document.querySelector(".rk-settings__label")).toBeNull();
    // The button, its section id and click handling stay.
    await user.click(screen.getByTestId("item-b"));
    expect(screen.getByTestId("item-b")).toHaveTextContent(">Beta");
    expect(screen.getByTestId("item-b").closest("button")).toHaveAttribute("data-section-id", "b");
  });
  it("hideNav renders only the body for the controlled section", async () => {
    panel({ hideNav: true, activeId: "b" });
    expect(await screen.findByText("beta body")).toBeInTheDocument();
    expect(screen.queryByRole("navigation")).toBeNull();
    expect(screen.getByTestId("settings-panel")).toHaveClass("rk-settings--no-nav");
  });
});

describe("updates adapter", () => {
  const upd = (over: Partial<UpdatesAdapter> = {}): UpdatesAdapter & { check: ReturnType<typeof vi.fn>; install: ReturnType<typeof vi.fn> } =>
    ({ check: vi.fn(async (): Promise<AutoUpdateStatus> => ({ kind: "up_to_date" })), install: vi.fn(async (): Promise<AutoUpdateStatus> => ({ kind: "ready", version: "2.0.0", tier: "patch" })), ...over }) as never;

  it("drives the section with a function-style adapter", async () => {
    const user = userEvent.setup();
    const client = upd({ check: vi.fn(async (): Promise<AutoUpdateStatus> => ({ kind: "available", version: "2.0.0", tier: "patch", eligible: true })) });
    render(<>{body(updatesSection({ client, currentVersion: "1.0.0" }))}</>);
    await user.click(screen.getByRole("button", { name: "Check now" }));
    await user.click(await screen.findByRole("button", { name: "Install update" }));
    expect(client.check).toHaveBeenCalledTimes(1);
    expect(client.install).toHaveBeenCalledTimes(1);
    expect(await screen.findByText("Version 2.0.0 is ready to install.")).toBeInTheDocument();
  });
  it("still accepts an AutoUpdateClient", async () => {
    const user = userEvent.setup();
    const client: AutoUpdateClient = { checkOnce: vi.fn(async () => ({ kind: "up_to_date" }) as const), installAvailable: vi.fn(async () => ({ kind: "idle" }) as const) };
    render(<>{body(updatesSection({ client, currentVersion: "1.0.0" }))}</>);
    await user.click(screen.getByRole("button", { name: "Check now" }));
    expect(await screen.findByText("You are up to date.")).toBeInTheDocument();
    expect(client.checkOnce).toHaveBeenCalledTimes(1);
  });
  it("seeds the display from adapter.status (value or function) on mount", async () => {
    const seed: AutoUpdateStatus = { kind: "available", version: "3.0.0", tier: "update", eligible: true };
    render(<>{body(updatesSection({ client: upd({ status: seed }), currentVersion: "1.0.0" }))}</>);
    expect(await screen.findByText("Version 3.0.0 is available (update).")).toBeInTheDocument();
    cleanup();
    render(<>{body(updatesSection({ client: upd({ status: async () => ({ kind: "up_to_date" }) }), currentVersion: "1.0.0" }))}</>);
    expect(await screen.findByText("You are up to date.")).toBeInTheDocument();
  });
  it("routes every string through labels, including in-flight labels and the badge", async () => {
    const user = userEvent.setup();
    let release: (s: AutoUpdateStatus) => void = () => {};
    const client = upd({ check: vi.fn(() => new Promise<AutoUpdateStatus>((r) => { release = r; })) });
    const labels = { checkNow: "Check for Updates", checking: "Checking…", installUpdate: "Install and restart", installing: "Installing…", statusBadge: (s: AutoUpdateStatus) => `S:${s.kind}`, status: (s: AutoUpdateStatus) => `St:${s.kind}` };
    render(<>{body(updatesSection({ client, currentVersion: "1.0.0", labels }))}</>);
    const check = screen.getByRole("button", { name: "Check for Updates" });
    expect(screen.getByText("S:idle")).toBeInTheDocument();
    await user.click(check);
    expect(await screen.findByRole("button", { name: "Checking…" })).toBeDisabled();
    release({ kind: "available", version: "2.0.0", tier: "patch", eligible: true });
    expect(await screen.findByRole("button", { name: "Install and restart" })).toBeEnabled();
    expect(screen.getByText("St:available")).toBeInTheDocument();
    expect(screen.queryByText("Check now")).toBeNull();
  });
  it("labels the section nav entry and search entry", () => {
    const s = updatesSection({ client: upd(), currentVersion: "1", labels: { sectionLabel: "Software update", searchCheck: "Look for updates" } });
    expect(s.label).toBe("Software update");
    expect(s.entries?.[0]?.label).toBe("Look for updates");
    expect(updatesSection({ client: upd(), currentVersion: "1" }).label).toBe("Updates");
  });
  it("shows an adapter error as an error status", async () => {
    const user = userEvent.setup();
    render(<>{body(updatesSection({ client: upd({ check: async () => { throw new Error("offline"); } }), currentVersion: "1.0.0" }))}</>);
    await user.click(screen.getByRole("button", { name: "Check now" }));
    expect(await screen.findByText("offline")).toBeInTheDocument();
  });
  it("does not reset its status when the adapter is recreated on re-render", async () => {
    const user = userEvent.setup();
    const make = () => upd({ status: { kind: "idle" } });
    const view = (c: UpdatesAdapter) => <UpdatesControls client={c} />;
    const { rerender } = render(view(make()));
    await user.click(screen.getByRole("button", { name: "Check now" }));
    await screen.findByText("You are up to date.");
    rerender(view(make()));
    expect(screen.getByText("You are up to date.")).toBeInTheDocument();
  });
});

describe("UpdatesControls (embedded)", () => {
  it("works standalone inside another card, with no Settings row", async () => {
    const user = userEvent.setup();
    const client: UpdatesAdapter = { check: async () => ({ kind: "available", version: "2.0.0", tier: "update", eligible: true }), install: async () => ({ kind: "ready", version: "2.0.0", tier: "update" }) };
    const onRestart = vi.fn();
    render(<div className="my-card"><h3>About</h3><UpdatesControls client={client} onRestart={onRestart} labels={{ checkNow: "Check for Updates", installUpdate: "Install and restart" }} /></div>);
    expect(document.querySelector(".rk-sr")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Check for Updates" }));
    await user.click(await screen.findByRole("button", { name: "Install and restart" }));
    await user.click(await screen.findByRole("button", { name: "Restart now" }));
    expect(onRestart).toHaveBeenCalledTimes(1);
  });
  it("can hide the status and share a controller with surrounding UI", async () => {
    const user = userEvent.setup();
    const client: UpdatesAdapter = { check: async () => ({ kind: "up_to_date" }), install: async () => ({ kind: "idle" }) };
    function Host() {
      const c = useUpdates(client);
      return <><p data-testid="outside">{c.status.kind}</p><UpdatesControls controller={c} showStatus={false} /></>;
    }
    render(<Host />);
    expect(document.querySelector(".rk-updates__status")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Check now" }));
    await waitFor(() => expect(screen.getByTestId("outside")).toHaveTextContent("up_to_date"));
  });
  it("requires a client or controller", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(() => render(<UpdatesControls />)).toThrow(/client or a controller/);
    spy.mockRestore();
  });
});

describe("license client adapter", () => {
  const active: StoredLicense = { status: "active", deviceId: "d", licenseKind: "perpetual" as never, validUntil: null };
  const make = (over: Partial<LicenseClientLike> = {}) => {
    const calls: string[] = [];
    const client: LicenseClientLike = {
      status: async () => active,
      activate: async (key) => { calls.push(`activate:${key}`); return active; },
      deactivate: async () => { calls.push("deactivate"); },
      ...over,
    };
    return { client, calls };
  };
  it("reads the current licence through status() and falls back to load()", async () => {
    const a = make();
    render(<>{body(licenseSection({ client: a.client }))}</>);
    expect(await screen.findByRole("button", { name: "Deactivate" })).toBeInTheDocument();
    cleanup();
    const b = make({ status: undefined, load: async () => active });
    render(<>{body(licenseSection({ client: b.client }))}</>);
    expect(await screen.findByRole("button", { name: "Deactivate" })).toBeInTheDocument();
    cleanup();
    const c = make({ status: undefined });
    render(<>{body(licenseSection({ client: c.client }))}</>);
    expect(await screen.findByLabelText("Licence key")).toBeInTheDocument();
  });
  it("activates through an app-private client and reports the licence", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const m = make({ status: async () => null });
    render(<>{body(licenseSection({ client: m.client, onChange }))}</>);
    await user.type(await screen.findByLabelText("Licence key"), "AAAA-BBBB");
    await user.click(screen.getByRole("button", { name: "Activate" }));
    await waitFor(() => expect(onChange).toHaveBeenCalledWith(active));
    expect(m.calls).toEqual(["activate:AAAA-BBBB"]);
  });
  it("awaits beforeDeactivate before deactivating", async () => {
    const user = userEvent.setup();
    const order: string[] = [];
    let unblock: () => void = () => {};
    const m = make({ deactivate: async () => { order.push("deactivate"); } });
    const beforeDeactivate = vi.fn(() => new Promise<void>((r) => { order.push("before"); unblock = () => { order.push("unblocked"); r(); }; }));
    render(<>{body(licenseSection({ client: m.client, beforeDeactivate }))}</>);
    await user.click(await screen.findByRole("button", { name: "Deactivate" }));
    await user.click(screen.getByRole("button", { name: "Confirm deactivate" }));
    await waitFor(() => expect(beforeDeactivate).toHaveBeenCalledTimes(1));
    expect(order).toEqual(["before"]);
    unblock();
    await waitFor(() => expect(order).toEqual(["before", "unblocked", "deactivate"]));
    expect(await screen.findByLabelText("Licence key")).toBeInTheDocument();
  });
  it("aborts the deactivation when beforeDeactivate throws and shows the error", async () => {
    const user = userEvent.setup();
    const deactivate = vi.fn(async () => {});
    render(<>{body(licenseSection({ client: make({ deactivate }).client, beforeDeactivate: () => { throw new Error("bridge busy"); } }))}</>);
    await user.click(await screen.findByRole("button", { name: "Deactivate" }));
    await user.click(screen.getByRole("button", { name: "Confirm deactivate" }));
    expect(await screen.findByText("bridge busy")).toBeInTheDocument();
    expect(deactivate).not.toHaveBeenCalled();
  });
  it("accepts a sync beforeDeactivate", async () => {
    const user = userEvent.setup();
    const hook = vi.fn();
    const m = make();
    render(<>{body(licenseSection({ client: m.client, beforeDeactivate: hook }))}</>);
    await user.click(await screen.findByRole("button", { name: "Deactivate" }));
    await user.click(screen.getByRole("button", { name: "Confirm deactivate" }));
    await waitFor(() => expect(m.calls).toEqual(["deactivate"]));
    expect(hook).toHaveBeenCalledTimes(1);
  });
});

describe("about card replacement", () => {
  const about = { appName: "Acme", version: "1.0.0", links: [{ label: "Website", url: "https://x.test" }], copyright: "(c) Acme" };
  it("default render is unchanged", () => {
    render(<>{body(aboutSection(about))}</>);
    expect(screen.getByText("Acme")).toBeInTheDocument();
    expect(screen.getByText("Website")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy version info" })).toBeInTheDocument();
  });
  it("card fully replaces the default card but keeps panels", () => {
    render(<>{body(aboutSection({ ...about, card: <div data-testid="mine">logo + updates</div>, panels: <aside data-testid="panels" /> }))}</>);
    expect(screen.getByTestId("mine")).toBeInTheDocument();
    expect(screen.getByTestId("panels")).toBeInTheDocument();
    expect(screen.queryByText("Website")).toBeNull();
    expect(screen.queryByRole("button", { name: "Copy version info" })).toBeNull();
  });
  it("render replaces everything and receives the defaults", () => {
    const render_ = vi.fn(({ card, panels, info }: { card: ReactNode; panels: ReactNode; info: string }) => (
      <div data-testid="custom"><span data-testid="info">{info}</span>{card}{panels}</div>
    ));
    render(<>{body(aboutSection({ ...about, build: "42", panels: <i data-testid="p" />, render: render_ }))}</>);
    expect(screen.getByTestId("info")).toHaveTextContent("Acme 1.0.0 · build 42");
    expect(screen.getByText("Website")).toBeInTheDocument();
    expect(screen.getByTestId("p")).toBeInTheDocument();
    cleanup();
    render(<>{body(aboutSection({ ...about, render: () => <p data-testid="only">only me</p> }))}</>);
    expect(screen.getByTestId("only")).toBeInTheDocument();
    expect(screen.queryByText("Website")).toBeNull();
  });
  it("embeds UpdatesControls in an app-owned card through the standard config", () => {
    const client: UpdatesAdapter = { check: async () => ({ kind: "up_to_date" }), install: async () => ({ kind: "idle" }) };
    const [s] = standardSections({ about: { ...about, card: <div><b>Logo</b><UpdatesControls client={client} /></div> }, include: ["about"] });
    render(<>{body(s!)}</>);
    expect(screen.getByText("Logo")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Check now" })).toBeInTheDocument();
  });
});

describe("licence labels, ids and one-click deactivate", () => {
  const active: StoredLicense = { status: "active", deviceId: "d", licenseKind: "perpetual" as never, validUntil: null };
  const mk = (over: Partial<LicenseClientLike> = {}) => {
    const deactivate = vi.fn(async () => {});
    return { deactivate, client: { status: async () => active, activate: async () => active, deactivate, ...over } as LicenseClientLike };
  };
  it("default output is unchanged (no ids, original strings, no wrapper)", async () => {
    const m = mk();
    const view = render(<>{body(licenseSection({ client: m.client }))}</>);
    expect(await screen.findByText("Deactivate this device")).toBeInTheDocument();
    expect(screen.getByText("Lifetime licence is active.")).toBeInTheDocument();
    expect(screen.getByText("Frees the seat so you can activate another device.")).toBeInTheDocument();
    expect(view.container.querySelector("[id]")).toBeNull();
    const s = licenseSection({ client: m.client });
    expect(s.label).toBe("Licence");
    expect(s.entries?.map((e) => e.label)).toEqual(["Licence status", "Activate licence key"]);
    cleanup();
    render(<>{body(licenseSection({ client: mk({ status: async () => null }).client }))}</>);
    expect(await screen.findByLabelText("Licence key")).toHaveAttribute("placeholder", "XXXX-XXXX-XXXX-XXXX");
    expect(screen.getByText("Paste the key from your purchase email.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Activate" })).toBeInTheDocument();
    expect(screen.getByText("No licence key is active on this device.")).toBeInTheDocument();
    expect(screen.getByText("Free")).toBeInTheDocument();
  });
  it("rewords the active state (ScrapeRight: Pro active / Lifetime / Deactivate this device)", async () => {
    const s = licenseSection({ client: mk().client, labels: { activeStatusLabel: "Pro active", lifetime: "Lifetime", deactivateTitle: "Deactivate this device", deactivateHint: "Deactivate to move your license to another computer.", deactivate: "Deactivate this device", activeDetail: (k) => `${k} Pro is active on this device`, sectionLabel: "Pro", searchStatus: "Plan" } });
    render(<>{body(s)}</>);
    expect(await screen.findByText("Pro active")).toBeInTheDocument();
    expect(screen.getByText("Pro is active on this device", { exact: false })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Deactivate this device" })).toBeInTheDocument();
    expect(screen.getByText("Deactivate to move your license to another computer.")).toBeInTheDocument();
    expect(s.label).toBe("Pro");
    expect(s.entries?.[0]?.label).toBe("Plan");
  });
  it("rewords the activate state, the free state, the busy label and the rejection", async () => {
    const user = userEvent.setup();
    let release: (l: StoredLicense) => void = () => {};
    const client = mk({ status: async () => null, activate: () => new Promise<StoredLicense>((r) => { release = r; }) }).client;
    render(<>{body(licenseSection({ client, purchaseUrl: "https://x.test", labels: { keyTitle: "Already bought Pro?", keyHint: "The key is in your purchase email.", keyAria: "License key", keyPlaceholder: "SR-XXXX-XXXX-XXXX", activate: "Activate", activating: "Activating…", free: "Plan: Free", freeDetail: "Free plan.", notAccepted: "License key was not accepted.", purchaseTitle: "Upgrade?", purchase: "Get Pro" } }))}</>);
    expect(await screen.findByText("Already bought Pro?")).toBeInTheDocument();
    expect(screen.getByText("Free plan.")).toBeInTheDocument();
    expect(screen.getByText("Plan: Free")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Get Pro" })).toBeInTheDocument();
    expect(screen.getByText("Upgrade?")).toBeInTheDocument();
    const input = screen.getByLabelText("License key");
    expect(input).toHaveAttribute("placeholder", "SR-XXXX-XXXX-XXXX");
    await user.type(input, "SR-1");
    await user.click(screen.getByRole("button", { name: "Activate" }));
    expect(await screen.findByRole("button", { name: "Activating…" })).toBeDisabled();
    release({ status: "failed" } as StoredLicense);
    expect(await screen.findByText("License key was not accepted.")).toBeInTheDocument();
  });
  it("rewords kinds, expired and rejected details", () => {
    const L = { trial: "Probe", subscription: "Term", expired: "Lapsed", expiredDetail: (k: string, u: string | null) => `${k} over ${u ? "on a date" : "now"}`, invalid: "Bad", rejectedDetail: "Nope." };
    render(<>{body(licenseSection({ client: mk({ status: async () => ({ status: "invalid" } as StoredLicense) }).client, labels: L }))}</>);
    return screen.findByText("Bad").then(() => {
      expect(screen.getByText("Nope.")).toBeInTheDocument();
    });
  });
  it("applies stable ids", async () => {
    const view = render(<>{body(licenseSection({ client: mk({ status: async () => null }).client, planSlot: <p>plan</p>, ids: { section: "settings-pro", licenseKey: "settings-license-key", form: "f", activate: "a", status: "s" } }))}</>);
    const root = view.container.querySelector("#settings-pro")!;
    expect(root).toContainElement(screen.getByText("plan"));
    expect(root.querySelector("#settings-license-key")).toBe(await screen.findByLabelText("Licence key"));
    expect(view.container.querySelector('#settings-pro button[type="submit"]')).toBe(root.querySelector("#a"));
    expect(root.querySelector("form#f")).not.toBeNull();
    expect(root.querySelector("#s")).toHaveAttribute("role", "status");
    cleanup();
    const v2 = render(<>{body(licenseSection({ client: mk().client, ids: { deactivate: "d" } }))}</>);
    await screen.findByText("Deactivate this device");
    expect(v2.container.querySelector("button#d")).not.toBeNull();
  });
  it("two-step by default; confirmDeactivate:false deactivates on the first click", async () => {
    const user = userEvent.setup();
    const a = mk();
    render(<>{body(licenseSection({ client: a.client }))}</>);
    await user.click(await screen.findByRole("button", { name: "Deactivate" }));
    expect(a.deactivate).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Confirm deactivate" })).toBeInTheDocument();
    cleanup();
    const b = mk();
    const onChange = vi.fn();
    render(<>{body(licenseSection({ client: b.client, confirmDeactivate: false, onChange }))}</>);
    await user.click(await screen.findByRole("button", { name: "Deactivate" }));
    await waitFor(() => expect(b.deactivate).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("button", { name: "Confirm deactivate" })).toBeNull();
    await waitFor(() => expect(onChange).toHaveBeenCalledWith(null));
    expect(await screen.findByLabelText("Licence key")).toBeInTheDocument();
  });
  it("one-click deactivate still honours beforeDeactivate failures", async () => {
    const user = userEvent.setup();
    const m = mk();
    render(<>{body(licenseSection({ client: m.client, confirmDeactivate: false, beforeDeactivate: () => { throw new Error("busy"); } }))}</>);
    await user.click(await screen.findByRole("button", { name: "Deactivate" }));
    expect(await screen.findByText("busy")).toBeInTheDocument();
    expect(m.deactivate).not.toHaveBeenCalled();
  });
});

describe("doctor section", () => {
  const mkChecks = (): { checks: DoctorCheck[]; runs: Record<string, ReturnType<typeof vi.fn>> } => {
    const runs = { a: vi.fn(async () => ({ ok: true, detail: "found" })), b: vi.fn(async () => ({ ok: false, detail: "missing", fix: "Install it" })), c: vi.fn(async () => { throw new Error("probe crashed"); }) };
    return { runs, checks: [{ id: "a", label: "Speech model", run: runs.a }, { id: "b", label: "ffmpeg", description: "Media tool", run: runs.b }, { id: "c", label: "OCR", run: runs.c }] };
  };
  it("renders idle: not-run badges, idle summary, descriptions, nothing run", () => {
    const { checks, runs } = mkChecks();
    render(<>{body(doctorSection({ checks }))}</>);
    expect(screen.getByText("Run the checks to see the engine status.")).toBeInTheDocument();
    expect(screen.getAllByText("not run")).toHaveLength(3);
    expect(screen.getByText("Media tool")).toBeInTheDocument();
    expect(runs.a).not.toHaveBeenCalled();
  });
  it("run all reports ok / failed / thrown, with detail, fix and summary", async () => {
    const user = userEvent.setup();
    const onResult = vi.fn();
    const { checks } = mkChecks();
    render(<>{body(doctorSection({ checks, onResult }))}</>);
    await user.click(screen.getByRole("button", { name: "Run all" }));
    expect(await screen.findByText("Last run: 1 of 3 passed.")).toBeInTheDocument();
    expect(screen.getByText("found")).toBeInTheDocument();
    expect(screen.getByText("missing")).toBeInTheDocument();
    expect(screen.getByText("Fix: Install it")).toBeInTheDocument();
    expect(screen.getByText("probe crashed")).toBeInTheDocument();
    expect(screen.getAllByText("needs attention")).toHaveLength(2);
    expect(screen.getByText("ok")).toBeInTheDocument();
    expect(onResult).toHaveBeenCalledTimes(3);
    expect(document.querySelector('[data-check-id="b"]')).toHaveAttribute("data-status", "fail");
  });
  it("shows a running state and disables run-all meanwhile", async () => {
    const user = userEvent.setup();
    let release: (r: { ok: boolean }) => void = () => {};
    render(<>{body(doctorSection({ checks: [{ id: "a", label: "A", run: () => new Promise((r) => { release = r; }) }] }))}</>);
    await user.click(screen.getByRole("button", { name: "Run all" }));
    expect(screen.getByText("Running diagnostics…")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Checking…" })[0]).toBeDisabled();
    expect(screen.getByText("checking")).toBeInTheDocument();
    release({ ok: true });
    expect(await screen.findByText("Last run: 1 of 1 passed.")).toBeInTheDocument();
  });
  it("re-runs a single check only", async () => {
    const user = userEvent.setup();
    const { checks, runs } = mkChecks();
    render(<>{body(doctorSection({ checks }))}</>);
    await user.click(screen.getByRole("button", { name: "Re-run ffmpeg" }));
    await screen.findByText("missing");
    expect(runs.b).toHaveBeenCalledTimes(1);
    expect(runs.a).not.toHaveBeenCalled();
    expect(screen.getByText("Last run: 0 of 3 passed.")).toBeInTheDocument();
  });
  it("autoRun runs every check once on mount", async () => {
    const { checks, runs } = mkChecks();
    render(<>{body(doctorSection({ checks, autoRun: true }))}</>);
    await screen.findByText("Last run: 1 of 3 passed.");
    expect(runs.a).toHaveBeenCalledTimes(1);
    expect(runs.c).toHaveBeenCalledTimes(1);
  });
  it("labels and ids are overridable", async () => {
    const user = userEvent.setup();
    const { checks } = mkChecks();
    const s = doctorSection({
      checks,
      labels: { sectionLabel: "Diagnostics", groupTitle: "Diagnostics", summaryTitle: "System check", idle: "Model, media tools and sign-in readiness.", runAll: "Run Check", runAllBusy: "Checking…", summary: (p, t) => `Last run: ${p} of ${t} passed!`, ok: "OK", fail: "not found", notRun: "-", detailsLabel: "Show details", fixPrefix: "Try: ", rerun: "Again", rerunAria: "Again:", searchLabel: "Run check" },
      ids: { section: "doctor", summary: "sum", runAll: "run", list: "lst", check: (id) => `chk-${id}`, rerun: (id) => `re-${id}` },
    });
    const view = render(<>{body(s)}</>);
    expect(s.label).toBe("Diagnostics");
    expect(s.entries?.[0]?.label).toBe("Run check");
    expect(screen.getByText("Model, media tools and sign-in readiness.")).toBeInTheDocument();
    expect(screen.getByText("Show details")).toBeInTheDocument();
    expect(view.container.querySelector("#doctor #sum")).not.toBeNull();
    expect(view.container.querySelector("#lst #chk-a")).not.toBeNull();
    await user.click(view.container.querySelector("#run")!);
    expect(await screen.findByText("Last run: 1 of 3 passed!")).toBeInTheDocument();
    expect(screen.getByText("OK")).toBeInTheDocument();
    expect(screen.getAllByText("not found")).toHaveLength(2);
    expect(screen.getByText("Try: Install it")).toBeInTheDocument();
    expect(view.container.querySelector("#re-b")).toHaveAttribute("aria-label", "Again: ffmpeg");
    expect(screen.getAllByText("Again")).toHaveLength(3);
  });
  it("is opt-in in standardSections and absent by default", () => {
    const { checks } = mkChecks();
    expect(standardSections({}).some((s) => s.id === "doctor")).toBe(false);
    expect(standardSections({ doctor: { checks } }).map((s) => s.id)).toContain("doctor");
    expect(standardSections({ doctor: { checks }, omit: ["doctor"] }).some((s) => s.id === "doctor")).toBe(false);
  });
  it("ignores a stale run that finishes after a newer one", async () => {
    const user = userEvent.setup();
    const resolvers: Array<(r: { ok: boolean; detail: string }) => void> = [];
    render(<>{body(doctorSection({ checks: [{ id: "a", label: "A", run: () => new Promise((r) => resolvers.push(r)) }], labels: { rerunBusy: "Re-run" } }))}</>);
    await user.click(screen.getByRole("button", { name: "Run all" }));
    resolvers[0]!({ ok: true, detail: "first" });
    await screen.findByText("first");
    await user.click(screen.getByRole("button", { name: "Re-run A" }));
    resolvers[1]!({ ok: false, detail: "second" });
    expect(await screen.findByText("second")).toBeInTheDocument();
    expect(screen.queryByText("first")).toBeNull();
  });
});
