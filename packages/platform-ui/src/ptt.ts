/**
 * Push-to-talk key matching for a webview that previews the hold-to-talk key. `code` is a
 * `KeyboardEvent.code`; `configuredKey` is the setting's key alias (`lalt`, `ralt`, `rctrl`,
 * the aliases of `rightkit-hotkeys` `PttAlias`). The two functions differ by platform because the
 * native hook treats an unknown alias differently: Windows falls back to right Alt, macOS to either Control.
 */
export function isWindowsPttKeyEvent(code: string, configuredKey: string): boolean {
  if (configuredKey === "lalt") return code === "AltLeft";
  return code === (configuredKey === "rctrl" ? "ControlRight" : "AltRight");
}

export function isMacPttKeyEvent(code: string, configuredKey: string): boolean {
  if (configuredKey === "lalt") return code === "AltLeft";
  if (configuredKey === "ralt") return code === "AltRight";
  return code === "ControlLeft" || code === "ControlRight";
}
