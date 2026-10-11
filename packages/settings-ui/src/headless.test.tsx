// @vitest-environment jsdom
// The headless hooks drive host markup that shares nothing with the styled parts (h2, custom classes, a list). Each case
// mirrors one the styled search tests cover: multi-term matching, no results, clearing, the search field and the group count.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { afterEach, describe, expect, it } from "vitest";
import {
  SettingsGroupScope,
  SettingsSearchProvider,
  SettingsSectionScope,
  defaultAnnouncement,
  useSettingsGroup,
  useSettingsItem,
  useSettingsRow,
  useSettingsSearchField,
  useSettingsSection,
} from "./headless.js";

afterEach(() => cleanup());

function HostRow({ id, label, description, keywords }: { id: string; label: string; description?: string; keywords?: readonly string[] }) {
  const r = useSettingsRow({ id, label, description, keywords });
  if (r.hidden) return null;
  return (
    <li className="my-row" {...r.props}>
      <b className="my-label">{r.mark(label)}</b>
      {description ? <span className="my-desc">{r.mark(description)}</span> : null}
    </li>
  );
}

/** The host decides what an empty group looks like; its children stay mounted either way so they keep registering. */
function HostGroup({ title, children }: { title: string; children: ReactNode }) {
  const g = useSettingsGroup();
  return (
    <SettingsGroupScope groupKey={g.key} title={title}>
      <i className="my-count">{g.visibleRowCount}</i>
      {g.empty ? (
        <>{children}</>
      ) : (
        <section className="my-card">
          <h2 className="my-heading">{title}</h2>
          <ul>{children}</ul>
        </section>
      )}
    </SettingsGroupScope>
  );
}

const highlight = (m: string) => <em className="my-hit">{m}</em>;

function Page({ query, filter }: { query: string; filter?: boolean }) {
  return (
    <SettingsSearchProvider query={query} filter={filter} highlight={highlight}>
      <HostGroup title="Appearance">
        <HostRow id="dark" label="Dark mode" description="Switch to a dark theme" keywords={["night"]} />
        <HostRow id="accent" label="Accent colour" />
      </HostGroup>
      <HostGroup title="Updates">
        <HostRow id="auto" label="Install updates automatically" />
      </HostGroup>
    </SettingsSearchProvider>
  );
}

const rowIds = (c: HTMLElement) => [...c.querySelectorAll("li")].map((li) => li.getAttribute("data-setting-id"));
const headings = (c: HTMLElement) => [...c.querySelectorAll("h2.my-heading")].map((h) => h.textContent);
const counts = (c: HTMLElement) => [...c.querySelectorAll(".my-count")].map((i) => i.textContent);

describe("headless row, group and provider with h2 and custom classes", () => {
  it("multi-term: every term must match the label, description, keywords or group title", () => {
    const v = render(<Page query="" />);
    const at = (q: string) => {
      v.rerender(<Page query={q} />);
      return rowIds(v.container);
    };
    expect(at("dark theme")).toEqual(["dark"]);
    expect(at("dark nope")).toEqual([]);
    expect(at("night")).toEqual(["dark"]);
    expect(at("updates")).toEqual(["auto"]);
    expect(at("dark")).toEqual(["dark"]);
    expect(at("")).toEqual(["dark", "accent", "auto"]);
  });

  it("marks data-hit on matching rows only while a query is active, and never hides them in filter=false mode", () => {
    const v = render(<Page query="dark" filter={false} />);
    expect(rowIds(v.container)).toEqual(["dark", "accent", "auto"]);
    const hits = [...v.container.querySelectorAll("li")].filter((li) => li.hasAttribute("data-hit")).map((li) => li.getAttribute("data-setting-id"));
    expect(hits).toEqual(["dark"]);
    v.rerender(<Page query="" />);
    expect(v.container.querySelector("[data-hit]")).toBeNull();
  });

  it("no results: every row is gone, every group is empty and its heading is not in the DOM", () => {
    const v = render(<Page query="zzz" />);
    expect(rowIds(v.container)).toEqual([]);
    expect(headings(v.container)).toEqual([]);
    expect(v.container.querySelector("section")).toBeNull();
    expect(counts(v.container)).toEqual(["0", "0"]);
  });

  it("clearing the query restores every row, heading and count", () => {
    const v = render(<Page query="zzz" />);
    v.rerender(<Page query="" />);
    expect(rowIds(v.container)).toEqual(["dark", "accent", "auto"]);
    expect(headings(v.container)).toEqual(["Appearance", "Updates"]);
    expect(counts(v.container)).toEqual(["2", "1"]);
  });

  it("groups decide after their rows register: headings show on first render with no query", () => {
    const v = render(<Page query="" />);
    expect(headings(v.container)).toEqual(["Appearance", "Updates"]);
  });

  it("a group counts only its own visible rows", () => {
    const v = render(<Page query="dark" />);
    expect(counts(v.container)).toEqual(["1", "0"]);
    expect(headings(v.container)).toEqual(["Appearance"]);
  });

  it("highlight is the host's own function: text content and attributes are unchanged", () => {
    const v = render(<Page query="dark theme" />);
    const label = v.container.querySelector("li .my-label");
    expect(label?.innerHTML).toBe('<span style="display: contents;"><em class="my-hit">Dark</em></span> mode');
    expect(label?.textContent).toBe("Dark mode");
    expect(v.container.querySelector("li .my-desc")?.innerHTML).toBe('Switch to a <span style="display: contents;"><em class="my-hit">dark</em></span> <span style="display: contents;"><em class="my-hit">theme</em></span>');
  });

  it("outside a provider the hooks are inert: rows render, nothing is hidden, no data-hit, groups are never empty", () => {
    render(
      <>
        <HostGroup title="Loose">
          <HostRow id="x" label="Loose row" />
        </HostGroup>
      </>,
    );
    expect(screen.getByText("Loose row")).toBeTruthy();
    expect(screen.getByRole("heading", { level: 2, name: "Loose" })).toBeTruthy();
    expect(document.querySelector("[data-hit]")).toBeNull();
  });

  it("props carry data-setting-id only when an id is given", () => {
    function Bare({ id }: { id?: string }) {
      const r = useSettingsRow({ id, label: "Bare" });
      return <div data-testid="bare" {...r.props} />;
    }
    const v = render(<SettingsSearchProvider query=""><Bare /><Bare id="z" /></SettingsSearchProvider>);
    const [a, b] = [...v.container.querySelectorAll("[data-testid=bare]")];
    expect(a!.hasAttribute("data-setting-id")).toBe(false);
    expect(b!.getAttribute("data-setting-id")).toBe("z");
  });
});

describe("headless item (content that is not a row)", () => {
  function HostItem({ id, text, keywords, children }: { id: string; text: string; keywords?: readonly string[]; children: ReactNode }) {
    const it = useSettingsItem({ id, text, keywords });
    if (it.hidden) return null;
    return <div className="my-item" {...it.props}>{children}</div>;
  }
  it("is shown only while it matches, carries data-hit, and counts for its group", () => {
    const at = (q: string) => (
      <SettingsSearchProvider query={q}>
        <HostGroup title="Accounts">
          <HostItem id="acct" text="Connected accounts mail" keywords={["signin"]}><button type="button">me@example.com</button></HostItem>
        </HostGroup>
      </SettingsSearchProvider>
    );
    const v = render(at("mail"));
    expect(screen.getByRole("button", { name: "me@example.com" })).toBeTruthy();
    expect(v.container.querySelector(".my-item")?.getAttribute("data-hit")).toBe("");
    expect(counts(v.container)).toEqual(["1"]);
    v.rerender(at("signin nope"));
    expect(screen.queryByRole("button", { name: "me@example.com" })).toBeNull();
    expect(counts(v.container)).toEqual(["0"]);
    v.rerender(at("signin"));
    expect(v.container.querySelector(".my-item")?.getAttribute("data-setting-id")).toBe("acct");
  });
});

describe("headless search field", () => {
  function SearchBox({ query, setQuery }: { query: string; setQuery(q: string): void }) {
    const f = useSettingsSearchField({ query, setQuery, delay: 0 });
    return (
      <>
        <input aria-label="Search" value={f.value} onChange={f.onChange} />
        <button type="button" onClick={f.clear}>Clear</button>
        <p role="status">{f.status}</p>
        <i data-testid="active">{String(f.active)}</i>
      </>
    );
  }
  function Search() {
    const [query, setQuery] = useState("");
    return (
      <SettingsSearchProvider query={query}>
        <SearchBox query={query} setQuery={setQuery} />
        <HostGroup title="Appearance">
          <HostRow id="dark" label="Dark mode" />
          <HostRow id="accent" label="Accent colour" />
        </HostGroup>
      </SettingsSearchProvider>
    );
  }

  it("value and onChange drive the query; status reports the count as an accessible sentence", () => {
    render(<Search />);
    const input = screen.getByLabelText("Search") as HTMLInputElement;
    expect(screen.getByRole("status").textContent).toBe("");
    fireEvent.change(input, { target: { value: "dark" } });
    expect(input.value).toBe("dark");
    expect(screen.getByRole("status").textContent).toBe("1 setting found");
    expect(screen.getByTestId("active").textContent).toBe("true");
    fireEvent.change(input, { target: { value: "zzz" } });
    expect(screen.getByRole("status").textContent).toBe("No settings found");
    expect(screen.queryByRole("heading", { level: 2, name: "Appearance" })).toBeNull();
  });

  it("clear empties the query, the status and the results", () => {
    render(<Search />);
    fireEvent.change(screen.getByLabelText("Search"), { target: { value: "accent" } });
    expect(screen.getByRole("status").textContent).toBe("1 setting found");
    act(() => screen.getByRole("button", { name: "Clear" }).click());
    expect((screen.getByLabelText("Search") as HTMLInputElement).value).toBe("");
    expect(screen.getByRole("status").textContent).toBe("");
    expect(screen.getByText("Dark mode")).toBeTruthy();
  });

  it("status is empty, not a claim, when the field sits outside the provider", () => {
    function Loose() {
      const [q, setQ] = useState("dark");
      const f = useSettingsSearchField({ query: q, setQuery: setQ });
      return <p role="status">{f.status}</p>;
    }
    render(<Loose />);
    expect(screen.getByRole("status").textContent).toBe("");
  });

  it("defaultAnnouncement is the sentence the status uses", () => {
    expect([0, 1, 4].map(defaultAnnouncement)).toEqual(["No settings found", "1 setting found", "4 settings found"]);
  });
});

describe("headless section", () => {
  function Section({ id, title, rows }: { id: string; title: string; rows: ReactNode }) {
    const s = useSettingsSection();
    return (
      <SettingsSectionScope scopeKey={s.key} id={id} title={title}>
        {s.empty ? (
          <>{rows}</>
        ) : (
          <div data-section={id}>
            <h3>{title}</h3>
            <ul>{rows}</ul>
          </div>
        )}
        <b data-testid={`count-${id}`}>{s.visibleRowCount}</b>
      </SettingsSectionScope>
    );
  }
  it("a host section shows its heading only while one of its rows matches", () => {
    const v = render(
      <SettingsSearchProvider query="dark">
        <Section id="look" title="Look" rows={<HostRow id="dark" label="Dark mode" />} />
        <Section id="sound" title="Sound" rows={<HostRow id="beep" label="Beep" />} />
      </SettingsSearchProvider>,
    );
    expect(v.container.querySelector("[data-section=look] h3")?.textContent).toBe("Look");
    expect(v.container.querySelector("[data-section=sound]")).toBeNull();
    expect(screen.getByTestId("count-look").textContent).toBe("1");
    expect(screen.getByTestId("count-sound").textContent).toBe("0");
    v.rerender(
      <SettingsSearchProvider query="zzz">
        <Section id="look" title="Look" rows={<HostRow id="dark" label="Dark mode" />} />
        <Section id="sound" title="Sound" rows={<HostRow id="beep" label="Beep" />} />
      </SettingsSearchProvider>,
    );
    expect(v.container.querySelector("h3")).toBeNull();
  });
});
