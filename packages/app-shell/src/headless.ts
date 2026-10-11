/**
 * `@rightkit/app-shell/headless`: behaviour-only controls for hosts that keep their own markup, classes and styles.
 * Imports only `react`; no stylesheet and no styled component.
 */
export { useToggle, useSegmentedControl } from "./react/headless.js";
export type {
  HeadlessToggleProps,
  UseToggleOptions,
  HeadlessSegmentedOption,
  HeadlessSegmentedMode,
  UseSegmentedControlOptions,
  HeadlessSegmentedGroupProps,
  HeadlessSegmentedOptionProps,
  HeadlessSegmentedControl,
} from "./react/headless.js";
