/**
 * Headless controls (0.6.4): behaviour, ARIA and key handling for a host that keeps its own markup, classes and styles.
 * Each hook returns props to spread on an element you render. It renders nothing and carries no class, style or id.
 * Imports only `react`.
 */
import { useState, type KeyboardEvent, type MouseEvent } from "react";

/** Props for one switch element. Use a `<button>`, or any element with `tabIndex` (keys are handled here). */
export interface HeadlessToggleProps {
  role: "switch";
  "aria-checked": boolean;
  "aria-label": string;
  "aria-disabled": boolean;
  tabIndex: number;
  onClick: (event: MouseEvent<Element>) => void;
  onKeyDown: (event: KeyboardEvent<Element>) => void;
}

export interface UseToggleOptions {
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** The accessible name. */
  label: string;
  disabled?: boolean;
}

/**
 * Switch behaviour. Space and Enter flip the value (and prevent the default, so a page does not scroll and a native
 * button does not click a second time). Disabled: no change on click or key, `tabIndex` -1, `aria-disabled` true.
 * On a native `<button>` the keys are handled here too, so do not add a second key handler.
 */
export function useToggle({ checked, onChange, label, disabled = false }: UseToggleOptions): HeadlessToggleProps {
  return {
    role: "switch",
    "aria-checked": checked,
    "aria-label": label,
    "aria-disabled": disabled,
    tabIndex: disabled ? -1 : 0,
    onClick: () => {
      if (!disabled) onChange(!checked);
    },
    onKeyDown: (event) => {
      if (disabled || (event.key !== " " && event.key !== "Spacebar" && event.key !== "Enter")) return;
      event.preventDefault();
      onChange(!checked);
    },
  };
}

export interface HeadlessSegmentedOption<T extends string> {
  value: T;
  disabled?: boolean;
}

export type HeadlessSegmentedMode = "radio" | "toggle";

export interface UseSegmentedControlOptions<T extends string> {
  value: T;
  options: readonly HeadlessSegmentedOption<T>[];
  onChange: (value: T) => void;
  /** Accessible name of the group. Required for `radio`; for `toggle` put the group in a `<fieldset>` with a legend instead. */
  label?: string;
  /** Disables every option. */
  disabled?: boolean;
  /**
   * `radio` (default): `role="radiogroup"`, one tab stop (roving tabindex) on the selected option, Arrow keys, Home and End
   * move and select (wrapping, skipping disabled options). `toggle`: a set of buttons with `aria-pressed`; each enabled
   * button is a tab stop and Tab moves between them.
   */
  mode?: HeadlessSegmentedMode;
}

export interface HeadlessSegmentedGroupProps {
  role: "radiogroup" | "group";
  "aria-label"?: string;
  "aria-disabled": boolean;
}

export interface HeadlessSegmentedOptionProps {
  /** `radio` mode only. */
  role?: "radio";
  /** `radio` mode only. */
  "aria-checked"?: boolean;
  /** `toggle` mode only. */
  "aria-pressed"?: boolean;
  "aria-disabled": boolean;
  tabIndex: number;
  onClick: (event: MouseEvent<Element>) => void;
  /** `radio` mode only. */
  onKeyDown?: (event: KeyboardEvent<Element>) => void;
  /**
   * Register the option element so arrow keys can move focus to it. Spread it with the other props (`ref` is a normal
   * prop in React 19 and is passed through in React 18 when spread). Without it, keys still select but do not move focus.
   */
  ref: (element: Element | null) => void;
}

export interface HeadlessSegmentedControl<T extends string> {
  groupProps: HeadlessSegmentedGroupProps;
  /** Props for the option whose value is `value`. */
  getOptionProps: (value: T) => HeadlessSegmentedOptionProps;
}

/** Segmented control behaviour for host markup: a radiogroup with roving focus, or a group of toggle buttons. */
export function useSegmentedControl<T extends string>({
  value,
  options,
  onChange,
  label,
  disabled = false,
  mode = "radio",
}: UseSegmentedControlOptions<T>): HeadlessSegmentedControl<T> {
  // Element per option value, filled by the `ref` callbacks. State keeps one Map for the component's lifetime.
  const [elements] = useState(() => new Map<T, Element>());
  const enabled = options.filter((option) => !disabled && option.disabled !== true);
  const roving = enabled.some((option) => option.value === value) ? value : enabled[0]?.value;
  const radio = mode === "radio";

  const groupProps: HeadlessSegmentedGroupProps = radio
    ? { role: "radiogroup", "aria-label": label, "aria-disabled": disabled }
    : { role: "group", "aria-label": label, "aria-disabled": disabled };

  const getOptionProps = (optionValue: T): HeadlessSegmentedOptionProps => {
    const option = options.find((candidate) => candidate.value === optionValue);
    const isDisabled = disabled || !option || option.disabled === true;
    const selected = value === optionValue;
    const ref = (element: Element | null) => {
      if (element) elements.set(optionValue, element);
      else elements.delete(optionValue);
    };
    const onClick = () => {
      if (!isDisabled) onChange(optionValue);
    };

    if (!radio) {
      return { "aria-pressed": selected, "aria-disabled": isDisabled, tabIndex: isDisabled ? -1 : 0, onClick, ref };
    }

    const onKeyDown = (event: KeyboardEvent<Element>) => {
      if (isDisabled) return;
      const count = enabled.length;
      if (count === 0) return;
      const index = enabled.findIndex((candidate) => candidate.value === optionValue);
      let next: T | undefined;
      switch (event.key) {
        case "ArrowRight":
        case "ArrowDown":
          next = enabled[(index + 1 + count) % count]?.value;
          break;
        case "ArrowLeft":
        case "ArrowUp":
          next = enabled[(index - 1 + count) % count]?.value;
          break;
        case "Home":
          next = enabled[0]?.value;
          break;
        case "End":
          next = enabled[count - 1]?.value;
          break;
        case " ":
        case "Spacebar":
          event.preventDefault();
          onChange(optionValue);
          return;
        default:
          return;
      }
      event.preventDefault();
      if (next === undefined) return;
      onChange(next);
      (elements.get(next) as HTMLElement | undefined)?.focus?.();
    };

    return {
      role: "radio",
      "aria-checked": selected,
      "aria-disabled": isDisabled,
      tabIndex: !isDisabled && optionValue === roving ? 0 : -1,
      onClick,
      onKeyDown,
      ref,
    };
  };

  return { groupProps, getOptionProps };
}
