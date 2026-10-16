// Headless toggle and segmented control: prop output and key behaviour. Hooks run inside a server render so no DOM is needed;
// option refs are called by hand with stand-in elements.
import { describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { useSegmentedControl, useToggle } from "../headless.js";

const key = (name: string) => ({ key: name, preventDefault: vi.fn() }) as unknown as import("react").KeyboardEvent<Element>;
const click = () => ({}) as unknown as import("react").MouseEvent<Element>;

/** Run a hook inside a render and hand back what it returned (a fresh state object per call, like one mounted component). */
function capture<T>(fn: () => T): T {
  let out: T | undefined;
  renderToStaticMarkup(createElement(() => {
    out = fn();
    return null;
  }));
  return out as T;
}

describe("useToggle", () => {
  it("returns switch props with no class, style or id", () => {
    const props = useToggle({ checked: true, onChange: () => undefined, label: "Wi-Fi" });
    expect(props).toMatchObject({ role: "switch", "aria-checked": true, "aria-label": "Wi-Fi", "aria-disabled": false, tabIndex: 0 });
    expect(Object.keys(props).sort()).toEqual(["aria-checked", "aria-disabled", "aria-label", "onClick", "onKeyDown", "role", "tabIndex"]);
  });

  it("click flips the value", () => {
    const onChange = vi.fn();
    useToggle({ checked: false, onChange, label: "x" }).onClick(click());
    expect(onChange).toHaveBeenCalledWith(true);
  });

  it("Space and Enter flip the value and prevent the default; other keys do nothing", () => {
    const onChange = vi.fn();
    const props = useToggle({ checked: true, onChange, label: "x" });
    const space = key(" ");
    props.onKeyDown(space);
    expect(onChange).toHaveBeenLastCalledWith(false);
    expect(space.preventDefault).toHaveBeenCalledTimes(1);
    const enter = key("Enter");
    props.onKeyDown(enter);
    expect(onChange).toHaveBeenLastCalledWith(false);
    expect(enter.preventDefault).toHaveBeenCalledTimes(1);
    const other = key("a");
    props.onKeyDown(other);
    expect(onChange).toHaveBeenCalledTimes(2);
    expect(other.preventDefault).not.toHaveBeenCalled();
  });

  it("disabled: tabIndex -1, aria-disabled, and no change from click or keys", () => {
    const onChange = vi.fn();
    const props = useToggle({ checked: false, onChange, label: "x", disabled: true });
    expect(props).toMatchObject({ "aria-disabled": true, tabIndex: -1 });
    props.onClick(click());
    const enter = key("Enter");
    props.onKeyDown(enter);
    expect(onChange).not.toHaveBeenCalled();
    expect(enter.preventDefault).not.toHaveBeenCalled();
  });
});

const options = [{ value: "a" }, { value: "b", disabled: true }, { value: "c" }, { value: "d" }] as const;

function radio(value: string, opts: readonly { value: string; disabled?: boolean }[] = options, disabled = false) {
  const onChange = vi.fn();
  const c = capture(() => useSegmentedControl({ value: value as "a", options: opts as never, onChange, label: "Mode", disabled, mode: "radio" }));
  const focused: string[] = [];
  for (const o of opts) {
    c.getOptionProps(o.value as "a").ref({ focus: () => focused.push(o.value) } as unknown as Element);
  }
  return { c, onChange, focused };
}

describe("useSegmentedControl, radio mode", () => {
  it("group is a radiogroup; options are radios with aria-checked", () => {
    const { c } = radio("c");
    expect(c.groupProps).toEqual({ role: "radiogroup", "aria-label": "Mode", "aria-disabled": false });
    expect(c.getOptionProps("c")).toMatchObject({ role: "radio", "aria-checked": true, tabIndex: 0 });
    expect(c.getOptionProps("a")).toMatchObject({ role: "radio", "aria-checked": false, tabIndex: -1 });
  });

  it("roving tabindex: only the selected enabled option is a tab stop; with none selected the first enabled one is", () => {
    expect(radio("d").c.getOptionProps("d").tabIndex).toBe(0);
    const unselected = radio("zzz").c;
    expect(unselected.getOptionProps("a").tabIndex).toBe(0);
    expect(unselected.getOptionProps("c").tabIndex).toBe(-1);
  });

  it("ArrowRight and ArrowDown select and focus the next enabled option, skipping disabled ones", () => {
    const { c, onChange, focused } = radio("a");
    c.getOptionProps("a").onKeyDown!(key("ArrowRight"));
    expect(onChange).toHaveBeenLastCalledWith("c");
    expect(focused).toEqual(["c"]);
    c.getOptionProps("c").onKeyDown!(key("ArrowDown"));
    expect(onChange).toHaveBeenLastCalledWith("d");
  });

  it("ArrowLeft and ArrowUp move backwards and wrap; skipped disabled options stay skipped", () => {
    const { c, onChange } = radio("a");
    c.getOptionProps("a").onKeyDown!(key("ArrowLeft"));
    expect(onChange).toHaveBeenLastCalledWith("d");
    c.getOptionProps("c").onKeyDown!(key("ArrowUp"));
    expect(onChange).toHaveBeenLastCalledWith("a");
  });

  it("Home and End select the first and last enabled options", () => {
    const { c, onChange, focused } = radio("c");
    c.getOptionProps("c").onKeyDown!(key("Home"));
    expect(onChange).toHaveBeenLastCalledWith("a");
    c.getOptionProps("c").onKeyDown!(key("End"));
    expect(onChange).toHaveBeenLastCalledWith("d");
    expect(focused).toEqual(["a", "d"]);
  });

  it("arrow keys prevent the default and other keys are ignored", () => {
    const { c, onChange } = radio("a");
    const arrow = key("ArrowRight");
    c.getOptionProps("a").onKeyDown!(arrow);
    expect(arrow.preventDefault).toHaveBeenCalled();
    const other = key("x");
    c.getOptionProps("a").onKeyDown!(other);
    expect(other.preventDefault).not.toHaveBeenCalled();
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("a disabled option ignores click and keys", () => {
    const { c, onChange } = radio("a");
    c.getOptionProps("b").onClick(click());
    c.getOptionProps("b").onKeyDown!(key("ArrowLeft"));
    expect(onChange).not.toHaveBeenCalled();
    expect(c.getOptionProps("b")).toMatchObject({ "aria-disabled": true, tabIndex: -1 });
  });

  it("click selects an enabled option", () => {
    const { c, onChange } = radio("a");
    c.getOptionProps("c").onClick(click());
    expect(onChange).toHaveBeenCalledWith("c");
  });

  it("group disabled disables every option", () => {
    const { c, onChange } = radio("a", options, true);
    expect(c.groupProps["aria-disabled"]).toBe(true);
    c.getOptionProps("c").onClick(click());
    expect(onChange).not.toHaveBeenCalled();
    expect(c.getOptionProps("c").tabIndex).toBe(-1);
  });
});

describe("useSegmentedControl, toggle mode", () => {
  it("group is a plain group with an optional name, and options are aria-pressed buttons with no roving", () => {
    const onChange = vi.fn();
    const c = capture(() => useSegmentedControl({ value: "a", options, onChange, mode: "toggle" }));
    expect(c.groupProps).toEqual({ role: "group", "aria-label": undefined, "aria-disabled": false });
    expect(c.getOptionProps("a")).toMatchObject({ "aria-pressed": true, "aria-disabled": false, tabIndex: 0 });
    expect(c.getOptionProps("c")).toMatchObject({ "aria-pressed": false, tabIndex: 0 });
    expect(c.getOptionProps("c").role).toBeUndefined();
    expect(c.getOptionProps("c").onKeyDown).toBeUndefined();
  });

  it("every enabled button is a tab stop; disabled ones are not and ignore clicks", () => {
    const onChange = vi.fn();
    const c = capture(() => useSegmentedControl({ value: "a", options, onChange, mode: "toggle" }));
    expect(c.getOptionProps("b")).toMatchObject({ "aria-disabled": true, tabIndex: -1 });
    c.getOptionProps("b").onClick(click());
    expect(onChange).not.toHaveBeenCalled();
    c.getOptionProps("d").onClick(click());
    expect(onChange).toHaveBeenCalledWith("d");
  });
});
