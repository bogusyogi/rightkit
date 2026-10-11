// @vitest-environment jsdom
// Byte-identity guard for 0.4.2. The expected strings were captured from the 0.4.1 build before any change in this release
// (the markup each call below renders), so every default path of the touched parts is pinned as literal HTML, not a snapshot
// that could be regenerated. Do not update these strings to match new output; a difference is a regression.
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { SettingRow, SettingsGroup } from "./primitives.js";
import { SettingsItem, SettingsSearchProvider, SettingsSearchSection, SettingsSectionScope } from "./search.js";

afterEach(() => cleanup());

const expected = {
  group: "<section class=\"rk-sg\"><h3 class=\"rk-sg__title\">A</h3><p class=\"rk-sg__desc\">D</p><div class=\"rk-sg__rows\"><div class=\"rk-sr rk-sr--stacked\" data-setting-id=\"r\"><div class=\"rk-sr__text\"><div class=\"rk-sr__label\">L</div><div class=\"rk-sr__desc\">x</div></div><div class=\"rk-sr__control\"><button>b</button></div></div></div></section>",
  groupHidden: "<section class=\"rk-sg\" style=\"display: none;\"><h3 class=\"rk-sg__title\">H</h3><div class=\"rk-sg__rows\"></div></section>",
  section: "<section class=\"rk-settings__result\" aria-labelledby=\"rk-search-sec-look\" data-section-id=\"look\" style=\"\"><div class=\"rk-settings__result-head\"><h2 id=\"rk-search-sec-look\" class=\"rk-settings__result-title\">Look</h2><button type=\"button\" class=\"rk-btn rk-btn--sm rk-btn--ghost\" aria-label=\"Open section Look\">Open section</button></div><div class=\"rk-sr\"><div class=\"rk-sr__text\"><div class=\"rk-sr__label\">Dark mode</div></div></div></section>",
  sectionEmpty: "<section class=\"rk-settings__result\" aria-labelledby=\"rk-search-sec-look\" data-section-id=\"look\" style=\"display: none;\"><div class=\"rk-settings__result-head\"><h3 id=\"rk-search-sec-look\" class=\"rk-settings__result-title\">Look</h3></div></section>",
  sectionNoProvider: "<section class=\"rk-settings__result c\" aria-labelledby=\"rk-search-sec-look\" data-section-id=\"look\"><div class=\"rk-settings__result-head\"><h2 id=\"rk-search-sec-look\" class=\"rk-settings__result-title\">Look</h2></div><p>x</p></section>",
  item: "<div class=\"k\" data-setting-id=\"a1\"><i>y</i></div>",
  scope: "",
};

describe("0.4.1 default markup, byte for byte", () => {
  it("SettingsGroup with title, description and a stacked row", () => {
    expect(render(<SettingsGroup title="A" description="D"><SettingRow id="r" label="L" description="x" stacked><button>b</button></SettingRow></SettingsGroup>).container.innerHTML).toBe(expected.group);
  });
  it("SettingsGroup hidden by hideWhenEmpty (true)", () => {
    expect(render(<SettingsSearchProvider query="zzz"><SettingsGroup title="H" hideWhenEmpty><SettingRow label="a" /></SettingsGroup></SettingsSearchProvider>).container.innerHTML).toBe(expected.groupHidden);
  });
  it("SettingsSearchSection with a match and an Open action", () => {
    expect(render(<SettingsSearchProvider query="dark"><SettingsSearchSection id="look" title="Look" onOpen={() => {}}><SettingRow label="Dark mode" /></SettingsSearchSection></SettingsSearchProvider>).container.innerHTML).toBe(expected.section);
  });
  it("SettingsSearchSection hidden, h3 heading", () => {
    expect(render(<SettingsSearchProvider query="zzz"><SettingsSearchSection id="look" title="Look" headingLevel={3}><SettingRow label="Dark mode" /></SettingsSearchSection></SettingsSearchProvider>).container.innerHTML).toBe(expected.sectionEmpty);
  });
  it("SettingsSearchSection without a provider keeps its section", () => {
    expect(render(<SettingsSearchSection id="look" title="Look" className="c"><p>x</p></SettingsSearchSection>).container.innerHTML).toBe(expected.sectionNoProvider);
  });
  it("SettingsItem inside a provider", () => {
    expect(render(<SettingsSearchProvider query="acc"><SettingsItem text="accounts" id="a1" className="k"><i>y</i></SettingsItem></SettingsSearchProvider>).container.innerHTML).toBe(expected.item);
  });
  it("SettingsSectionScope renders no DOM", () => {
    expect(render(<SettingsSearchProvider query="zz"><SettingsSectionScope id="s" title="S"><SettingRow label="q" /></SettingsSectionScope></SettingsSearchProvider>).container.innerHTML).toBe(expected.scope);
  });
});
