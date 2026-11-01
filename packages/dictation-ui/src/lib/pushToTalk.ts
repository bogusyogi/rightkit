// Push-to-talk key choices per platform. "off" is a UI-only value: it maps to
// `push_to_talk: false` and leaves the stored key untouched.
export const MAC_PTT_KEY_OPTIONS = [
  { value: "ctrl", label: "Control", title: "Either Control key" },
  { value: "lalt", label: "Left ⌥", title: "Left Option" },
  { value: "ralt", label: "Right ⌥", title: "Right Option" },
] as const;

export const WINDOWS_PTT_KEY_OPTIONS = [
  { value: "lalt", label: "Left Alt", title: "Left Alt" },
  { value: "ralt", label: "Right Alt", title: "Right Alt" },
  { value: "rctrl", label: "Right Ctrl", title: "Right Control" },
] as const;
