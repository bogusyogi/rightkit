/** Native drag-drop phases reported by Tauri's `onDragDropEvent` (`DragDropEvent.type`). */
export type NativeDragPhase = "enter" | "over" | "drop" | "leave";

/**
 * Overlay visibility after one native drag-drop event. Tauri's native drag
 * handling (enabled by default) suppresses the HTML5 dragenter/dragleave
 * events on Windows, so the overlay follows the native phases instead:
 * `enter` and `over` show it; `leave` (drag left the window) and `drop` hide it.
 */
export function nextDropOverlayVisible(_visible: boolean, phase: NativeDragPhase): boolean {
  return phase === "enter" || phase === "over";
}
