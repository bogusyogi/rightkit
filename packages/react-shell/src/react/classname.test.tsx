// 0.6.4: `className` is added after the built-in class; without it the markup is exactly what it was in 0.6.3.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SegmentedControl, Toggle } from "./ui.js";

const noop = () => undefined;

describe("Toggle className", () => {
  it("default markup is unchanged", () => {
    expect(renderToStaticMarkup(<Toggle checked label="Wi-Fi" onChange={noop} />)).toBe(
      '<button type="button" role="switch" aria-checked="true" aria-label="Wi-Fi" class="rk-toggle"><span class="rk-toggle__knob"></span></button>',
    );
  });

  it("className is added after rk-toggle on the root only", () => {
    expect(renderToStaticMarkup(<Toggle checked={false} label="Wi-Fi" onChange={noop} className="mine other" disabled />)).toBe(
      '<button type="button" role="switch" aria-checked="false" aria-label="Wi-Fi" disabled="" class="rk-toggle mine other"><span class="rk-toggle__knob"></span></button>',
    );
  });
});

describe("SegmentedControl className", () => {
  const options = [{ value: "a", label: "A" }, { value: "b", label: "B" }] as const;

  it("default markup is unchanged", () => {
    expect(renderToStaticMarkup(<SegmentedControl value="a" options={options} onChange={noop} label="Mode" />)).toBe(
      '<div class="rk-seg" role="radiogroup" aria-label="Mode"><button type="button" role="radio" aria-checked="true" class="rk-seg__item is-on">A</button><button type="button" role="radio" aria-checked="false" class="rk-seg__item">B</button></div>',
    );
  });

  it("className is added after rk-seg on the root only", () => {
    const html = renderToStaticMarkup(<SegmentedControl value="b" options={options} onChange={noop} label="Mode" className="wide" />);
    expect(html.startsWith('<div class="rk-seg wide" role="radiogroup" aria-label="Mode">')).toBe(true);
    expect(html).toContain('class="rk-seg__item is-on"');
    expect(html.match(/wide/g)).toHaveLength(1);
  });
});
