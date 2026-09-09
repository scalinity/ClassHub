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
  /** Where the grade could land, once a score exists (SPEC §11). */
  projection: Projection | null;
  /** ISO start of the final exam, when scheduled (dashboard countdown chip). */
  finalExamStart: string | null;
  /**
   * Where the course is today, in its own words (SPEC §8.5) — resolved from
   * its published schedule, or for a course that publishes no dates from the
   * week its latest lecture was filed into, which `week` then names; never
   * computed.
   */
  currentUnit: { id: number; name: string; week: number | null } | null;
  meetings: Meeting[];
}

/** Mirrors `grades.rs::Projection`. */
export interface Projection {
  current: number;
  letter: string;
  /** Percentage points of the final grade already banked. */
  earned: number;
  /** Percentage points still open. */
  open: number;
  floor: number;
  ceiling: number;
  nextLetter: string | null;
  needed: number | null;
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


/**
 * `Week 3 — Data Exploration` → `{ label: "Week 3", topic: "Data Exploration" }`:
 * the card's meta line and its headline. A division with no separator is all
 * label and no topic (`Reading Days — No Class (Reading Days)` keeps its own
 * dash-separated words as the topic, exactly as the syllabus wrote them).
 */
export function splitUnitName(name: string): { label: string; topic: string | null } {
  const at = DIVISION_SEPARATOR.exec(name);
  if (at === null || at.index === 0) return { label: name, topic: null };
  return {
    label: name.slice(0, at.index),
    topic: name.slice(at.index + at[0].length) || null,
  };
}

/** Class color name -> the CSS accent token (per-card `--accent` pattern). */
export const CLASS_ACCENTS: Record<string, string> = {
  blue: "var(--class-blue)",
  orange: "var(--class-orange)",
  green: "var(--class-green)",
  amber: "var(--class-amber)",
};
