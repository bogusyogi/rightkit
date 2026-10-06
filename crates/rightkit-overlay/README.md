# rightkit-overlay

Shared overlay policy for HeardRight's pill and Cockpit's notch. The crate owns serializable state,
work-area placement, hover hit testing, and locked macOS status-level policy. On Windows, native
hosts pass an `HWND` as `isize` to `windows::make_noactivate` before first show, then call
`windows::reassert_if_needed` from their visible-overlay watchdog. This applies the donor's
`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` styles, preserves focus with `SWP_NOACTIVATE`, and repairs
actual `HWND_TOPMOST` z-order drift rather than checking only a policy bit. Native hosts still own
window creation and rendering.
