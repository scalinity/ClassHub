import { invoke } from "@tauri-apps/api/core";

export interface Meeting {
  weekday: number; // 1=Mon .. 7=Sun
  startTime: string; // "16:05"
  endTime: string; // "19:05"
}

export interface ClassInfo {
  id: number;
  displayName: string;
  color: string; // blue | orange | green | amber
  room: string;
  instructors: string;
  credits: number;
  folderName: string;
  folderPresent: boolean;
  /** Guides whose sources changed since generation (card badge). */
  staleGuides: number;
  /** Items waiting in the drop-to-sort flow: pending proposals + unproposed inbox files. */
  inboxPending: number;
  /** Nearest open deadline (card line), overdue included. */
  nextDeadline: { title: string; dueAt: string } | null;
  /** ISO start of the final exam, when scheduled (dashboard countdown chip). */
  finalExamStart: string | null;
  meetings: Meeting[];
}

export function listClasses(): Promise<ClassInfo[]> {
  return invoke<ClassInfo[]>("list_classes");
}

/** Class color name -> the CSS accent token (per-card `--accent` pattern). */
export const CLASS_ACCENTS: Record<string, string> = {
  blue: "var(--class-blue)",
  orange: "var(--class-orange)",
  green: "var(--class-green)",
  amber: "var(--class-amber)",
};
