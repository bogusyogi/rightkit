export function isWindowsPttKeyEvent(code: string, configuredKey: string): boolean {
  if (configuredKey === "lalt") return code === "AltLeft";
  return code === (configuredKey === "rctrl" ? "ControlRight" : "AltRight");
}

export function isMacPttKeyEvent(code: string, configuredKey: string): boolean {
  if (configuredKey === "lalt") return code === "AltLeft";
  if (configuredKey === "ralt") return code === "AltRight";
  return code === "ControlLeft" || code === "ControlRight";
}
