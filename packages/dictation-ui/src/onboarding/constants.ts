export const BUTTON =
  "inline-flex min-h-[38px] items-center justify-center rounded-[var(--radius-md)] border border-[var(--paper)] bg-[var(--paper)] px-4 text-[13px] font-bold text-[var(--surface-2)] transition-[background-color,transform,opacity] duration-150 hover:bg-[var(--ink-2)] active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-45 disabled:active:scale-100 focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";
// One primary style: the same --accent fill and --accent-contrast ink as the RightKit footer primary (see index.css).
export const BUTTON_PRIMARY =
  "!border-[var(--accent)] !bg-[var(--accent)] !text-[var(--accent-contrast)] hover:!bg-[var(--accent-hover)]";
export const BUTTON_SECONDARY =
  "!border-[var(--line)] !bg-[var(--surface-2)] !text-[var(--paper)] hover:!bg-[var(--surface-1)]";
export const ACTION_BAR =
  "grid grid-cols-[1fr_auto_1fr] items-center gap-3 bg-transparent px-6 pb-5 pt-1 max-[760px]:px-4";
// `hr-step` is a class hook the host's stylesheet defines: it top-aligns the step and sets its 16px gap.
export const STEP_SURFACE =
  "hr-step mx-auto grid h-full w-full max-w-[920px] content-start pb-6 text-center";
export const SIMPLE_ROW =
  "grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4 border-b border-[var(--line)] py-3 text-left last:border-b-0";
export const PLAIN_SECTION = "mx-auto w-full max-w-[820px]";
export const METER_BARS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
// The one privacy sentence pair for setup and the legal gate. Audio never leaves the device; text goes out only
// when the user turns on AI polish.
export const PRIVACY_LINE = "Your audio never leaves your device. Text goes out only if you turn on AI polish.";
