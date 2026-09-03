import { invoke } from "@tauri-apps/api/core";

import { todayIso } from "./schedule";

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
  /** Proposed deadlines still waiting on a decision, from the syllabus scan or Canvas. */
  pendingDeadlineProposals: number;
  /** Nearest open deadline (card line), overdue included. */
  nextDeadline: { title: string; dueAt: string } | null;
  /** Current weighted grade over graded items (null until something is graded). */
  currentGrade: number | null;
  /** ISO start of the final exam, when scheduled (dashboard countdown chip). */
  finalExamStart: string | null;
  /**
   * Where the course is today, in its own words (SPEC §8.5) — resolved from
   * its published schedule, never computed, so a course that publishes no
   * dates has none.
   */
  currentUnit: { id: number; name: string } | null;
  meetings: Meeting[];
}

/** `today` is YYYY-MM-DD from the same clock as the card's meeting and
 *  deadline labels; the current division is measured against it. */
export function listClasses(today: string): Promise<ClassInfo[]> {
  return invoke<ClassInfo[]>("list_classes", { today });
}

/**
 * The one classes query every screen shares. Today rides in the key as well
 * as the request: the answer depends on the day, so a dashboard left open
 * across midnight fetches again under the new day instead of serving
 * yesterday's division, and every `["classes"]` invalidation still matches
 * by prefix.
 */
export function classesQuery() {
  const today = todayIso();
  return {
    queryKey: ["classes", today] as const,
    queryFn: () => listClasses(today),
  };
}

/** A division's own separator between its number and its topic — an em or en
 *  dash, a spaced hyphen, or the colon a Part uses. */
const DIVISION_SEPARATOR = /\s+[—–-]\s+|:\s+/;

/** `Week 3 — Data Exploration` → `WEEK 3 · DATA EXPLORATION`: the card's line. */
export function currentUnitLabel(name: string): string {
  return name.replace(DIVISION_SEPARATOR, " · ").toUpperCase();
}

/** Class color name -> the CSS accent token (per-card `--accent` pattern). */
export const CLASS_ACCENTS: Record<string, string> = {
  blue: "var(--class-blue)",
  orange: "var(--class-orange)",
  green: "var(--class-green)",
  amber: "var(--class-amber)",
};
