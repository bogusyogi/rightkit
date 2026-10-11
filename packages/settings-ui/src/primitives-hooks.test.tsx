// 0.4.4: class hooks, the lead slot and the bare control. Default markup must not change, so the defaults are literal strings.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SettingRow, SettingsGroup } from "./primitives.js";

describe("SettingsGroup hooks", () => {
  it("default markup is unchanged", () => {
    expect(renderToStaticMarkup(<SettingsGroup title="A" description="D"><i>x</i></SettingsGroup>)).toBe(
      '<section class="rk-sg"><h3 class="rk-sg__title">A</h3><p class="rk-sg__desc">D</p><div class="rk-sg__rows"><i>x</i></div></section>',
    );
  });

  it("title, hint and rows classes are added after the built-in ones; the root class is added as before", () => {
    expect(
      renderToStaticMarkup(
        <SettingsGroup title="A" description="D" className="c" titleClassName="t" hintClassName="h" rowsClassName="r"><i>x</i></SettingsGroup>,
      ),
    ).toBe('<section class="rk-sg c"><h3 class="rk-sg__title t">A</h3><p class="rk-sg__desc h">D</p><div class="rk-sg__rows r"><i>x</i></div></section>');
  });
});

describe("SettingRow hooks", () => {
  it("default markup is unchanged", () => {
    expect(renderToStaticMarkup(<SettingRow id="r" label="L" description="x"><button>b</button></SettingRow>)).toBe(
      '<div class="rk-sr" data-setting-id="r"><div class="rk-sr__text"><div class="rk-sr__label">L</div><div class="rk-sr__desc">x</div></div><div class="rk-sr__control"><button>b</button></div></div>',
    );
  });

  it("stacked keeps its class and order", () => {
    expect(renderToStaticMarkup(<SettingRow label="L" stacked><button>b</button></SettingRow>)).toBe(
      '<div class="rk-sr rk-sr--stacked"><div class="rk-sr__text"><div class="rk-sr__label">L</div></div><div class="rk-sr__control"><button>b</button></div></div>',
    );
  });

  it("className, title and hint classes, the lead slot and the bare control", () => {
    expect(
      renderToStaticMarkup(
        <SettingRow id="r" label="L" description="x" className="k" titleClassName="t" hintClassName="h" lead={<span>I</span>} bareControl>
          <button>b</button>
        </SettingRow>,
      ),
    ).toBe(
      '<div class="rk-sr rk-sr--lead k" data-setting-id="r"><div class="rk-sr__lead"><span>I</span></div><div class="rk-sr__text"><div class="rk-sr__label t">L</div><div class="rk-sr__desc h">x</div></div><button>b</button></div>',
    );
  });

  it("no lead and no bare: no lead wrapper and no lead class", () => {
    const html = renderToStaticMarkup(<SettingRow label="L" lead={null}><button>b</button></SettingRow>);
    expect(html).not.toContain("rk-sr__lead");
    expect(html).not.toContain("rk-sr--lead");
    expect(html).toContain('<div class="rk-sr__control">');
  });
});
