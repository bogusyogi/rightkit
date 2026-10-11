// @vitest-environment jsdom
// 0.4.4: `enabled: false` makes search inert (no registration, no store update, no subscriber render); the group option
// `hideOnlyWhileSearching`; and the once-per-page warning for a row whose label cannot be searched.
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  SettingsGroupScope,
  SettingsSearchProvider,
  SettingsSectionScope,
  searchStoreStats,
  useSettingsGroup,
  useSettingsHit,
  useSettingsItem,
  useSettingsRow,
  useSettingsSearch,
  useSettingsSection,
  type SettingsGroupOptions,
  type SettingsGroupState,
  type SettingsRowState,
} from "./headless.js";

afterEach(() => cleanup());

function Row({ id, label, enabled, out }: { id: string; label: string; enabled?: boolean; out?: { state?: SettingsRowState } }) {
  const r = useSettingsRow({ id, label, enabled });
  if (out) out.state = r;
  return r.hidden ? null : <li {...r.props}>{label}</li>;
}

describe("search off: zero store updates and no subscriber renders", () => {
  it("a provider with enabled=false mounts rows, groups, sections, items and hits without touching the store", () => {
    let subscriberRenders = 0;
    function Subscriber() {
      subscriberRenders += 1;
      useSettingsSearch();
      return null;
    }
    function Group() {
      const g = useSettingsGroup();
      const s = useSettingsSection();
      const item = useSettingsItem({ text: "Account" });
      const hit = useSettingsHit({ text: "Help" });
      return (
        <SettingsSectionScope id="look" title="Look" scopeKey={s.key}>
          <SettingsGroupScope groupKey={g.key} title="Appearance">
            <Row id="dark" label="Dark mode" />
            <Row id="accent" label="Accent colour" />
          </SettingsGroupScope>
          <span>{item.matches ? "item" : "no"}{hit.matches ? "hit" : ""}</span>
        </SettingsSectionScope>
      );
    }
    const before = searchStoreStats.updates;
    const v = render(
      <SettingsSearchProvider query="dark" enabled={false} filter>
        <Subscriber />
        <Group />
      </SettingsSearchProvider>,
    );
    expect(searchStoreStats.updates - before).toBe(0);
    expect(subscriberRenders).toBe(1);
    expect(v.container.querySelectorAll("li")).toHaveLength(2);
    expect(v.container.querySelector("[data-hit]")).toBeNull();
  });

  it("the same tree with search on does update the store, so the counter is live", () => {
    const before = searchStoreStats.updates;
    render(
      <SettingsSearchProvider query="">
        <Row id="a" label="Alpha" />
      </SettingsSearchProvider>,
    );
    expect(searchStoreStats.updates - before).toBeGreaterThan(0);
  });

  it("a hook with enabled=false inside an enabled provider registers nothing and is inert", () => {
    const before = searchStoreStats.updates;
    const out: { state?: SettingsRowState } = {};
    const v = render(
      <SettingsSearchProvider query="zzz">
        <Row id="x" label="Dark mode" enabled={false} out={out} />
      </SettingsSearchProvider>,
    );
    expect(searchStoreStats.updates - before).toBe(0);
    expect(out.state).toMatchObject({ hidden: false, matches: true });
    expect(out.state?.props).toEqual({ "data-setting-id": "x" });
    expect(v.container.querySelectorAll("li")).toHaveLength(1);
  });

  it("an inert group is never empty, so its host wrapper is not hidden", () => {
    let group: SettingsGroupState | undefined;
    function G() {
      group = useSettingsGroup({ enabled: false });
      return null;
    }
    render(
      <SettingsSearchProvider query="zzz">
        <G />
      </SettingsSearchProvider>,
    );
    expect(group).toMatchObject({ empty: false, hidden: false, props: {} });
  });
});

describe("useSettingsGroup and useSettingsSection: hideOnlyWhileSearching", () => {
  function Page({ query, opts, withRow }: { query: string; opts?: SettingsGroupOptions; withRow: boolean }) {
    return (
      <SettingsSearchProvider query={query}>
        <Group opts={opts} withRow={withRow} />
      </SettingsSearchProvider>
    );
  }
  let last: SettingsGroupState | undefined;
  function Group({ opts, withRow }: { opts?: SettingsGroupOptions; withRow: boolean }) {
    const g = useSettingsGroup(opts);
    last = g;
    return <SettingsGroupScope groupKey={g.key} title="Appearance">{withRow ? <Row id="dark" label="Dark mode" /> : <i>plain</i>}</SettingsGroupScope>;
  }

  it("default: a group with no rows is empty with no query (today's behaviour)", () => {
    render(<Page query="" withRow={false} />);
    expect(last?.empty).toBe(true);
  });

  it("hideOnlyWhileSearching: a plain-content group stays visible when not searching", () => {
    render(<Page query="" opts={{ hideOnlyWhileSearching: true }} withRow={false} />);
    expect(last).toMatchObject({ empty: false, hidden: false, props: {} });
  });

  it("hideOnlyWhileSearching: empty while a query is active and no row matches, visible when one does", () => {
    const v = render(<Page query="zzz" opts={{ hideOnlyWhileSearching: true }} withRow />);
    expect(last?.empty).toBe(true);
    expect(last?.props).toEqual({ hidden: true, style: { display: "none" }, "data-empty": "" });
    v.rerender(<Page query="dark" opts={{ hideOnlyWhileSearching: true }} withRow />);
    expect(last?.empty).toBe(false);
    expect(last?.visibleRowCount).toBe(1);
  });

  it("hideOnlyWhileSearching: a plain-content group is empty while searching with no matching rows", () => {
    render(<Page query="zzz" opts={{ hideOnlyWhileSearching: true }} withRow={false} />);
    expect(last?.empty).toBe(true);
  });

  it("the section hook takes the same option", () => {
    let section: ReturnType<typeof useSettingsSection> | undefined;
    function S({ query }: { query: string }) {
      section = useSettingsSection({ hideOnlyWhileSearching: true });
      return null;
    }
    const v = render(<SettingsSearchProvider query=""><S query="" /></SettingsSearchProvider>);
    expect(section?.empty).toBe(false);
    v.rerender(<SettingsSearchProvider query="zzz"><S query="zzz" /></SettingsSearchProvider>);
    expect(section?.empty).toBe(true);
  });
});

describe("non-string row labels", () => {
  it("warns once in development when a row has a non-string label and no searchText", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      render(
        <SettingsSearchProvider query="">
          <FancyRow id="a" />
          <FancyRow id="b" />
        </SettingsSearchProvider>,
      );
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]?.[0])).toContain("searchText");
    } finally {
      warn.mockRestore();
    }
  });
  function FancyRow({ id }: { id: string }) {
    useSettingsRow({ id, label: <b>Fancy</b> });
    return null;
  }
});
