import type { MouseEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * Explicit window dragging for chrome areas (the top strip, viewer headers).
 * `data-tauri-drag-region` only fires when the mousedown target IS the marked
 * element, so any child under the cursor silently defeats it; calling
 * startDragging ourselves makes the whole area drag except interactive
 * controls.
 */
export function dragWindow(e: MouseEvent) {
  if (e.buttons !== 1) return;
  if ((e.target as HTMLElement).closest("button, a, input, select, summary")) {
    return;
  }
  void getCurrentWindow().startDragging();
}
