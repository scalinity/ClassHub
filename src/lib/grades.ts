import { invoke } from "@tauri-apps/api/core";

import type { Projection } from "@/lib/classes";

export interface GradeItem {
  id: number;
  name: string;
  score: number;
  maxScore: number;
  /** Chat's optional field; the UI form leaves it untouched. */
  gradedAt: string | null;
  /** Set when a Canvas sync recorded it — the next sync writes Canvas's score
   *  back over any hand edit. */
  canvasAssignmentId: string | null;
}

export interface GradeCategory {
  id: number;
  name: string;
  weight: number;
  /** Points earned across this category's items, when any exist. */
  percent: number | null;
  items: GradeItem[];
  /** Set when this category is one of the course's Canvas assignment groups. */
  canvasGroupId: string | null;
}

/** The tag a category or item the Canvas sync owns carries — the deadline
 *  row's "from Canvas", with what it means here. A hand edit is allowed and
 *  audited; the tag is what says it will not outlast the next sync. */
export const CANVAS_CATEGORY_TAG = {
  label: "from Canvas",
  title:
    "One of the course's Canvas assignment groups — the sync keeps its name, and its weight where the course weights groups",
};
export const CANVAS_ITEM_TAG = {
  label: "from Canvas",
  title:
    "Read from Canvas — an edit here lasts until the next sync, which writes Canvas's score back",
};

export interface GradesInfo {
  categories: GradeCategory[];
  weightTotal: number;
  /** SPEC §11 math — null until at least one item is recorded. */
  currentGrade: number | null;
  /** Where the grade could land, once a score exists (SPEC §11). */
  projection: Projection | null;
}

/** `61–91%` — where the final grade could land, floor to ceiling. */
export function formatRange(projection: Projection): string {
  return `${Math.round(projection.floor)}–${Math.round(projection.ceiling)}%`;
}

/**
 * The Grades section's one line (SPEC §11): the current grade and its
 * letter, how much of the grade is still open, and what the open share has
 * to average for the next letter up — `out of reach` past full marks.
 */
export function projectionLine(projection: Projection): string {
  const parts = [`Current ${formatPercent(projection.current)} · ${projection.letter}`];
  if (projection.open > 0) {
    parts.push(`${formatPercent(projection.open)} of the grade still open`);
    parts.push(`could land ${formatRange(projection)}`);
    if (projection.nextLetter !== null && projection.needed !== null) {
      parts.push(
        projection.needed > 100
          ? `${projection.nextLetter} is out of reach`
          : `the rest needs to average ${formatPercent(projection.needed)} for ${projection.nextLetter === "A" ? "an" : "a"} ${projection.nextLetter}`,
      );
    }
  } else {
    parts.push("nothing left open");
  }
  return parts.join(" · ");
}

export function listGrades(classId: number): Promise<GradesInfo> {
  return invoke<GradesInfo>("list_grades", { classId });
}

/** id undefined creates; an id amends. */
export function saveGradeCategory(args: {
  classId: number;
  id?: number;
  name: string;
  weight: number;
}): Promise<void> {
  return invoke("save_grade_category", args);
}

/** The category and all its items ride the audit log — recoverable. */
export function deleteGradeCategory(id: number): Promise<void> {
  return invoke("delete_grade_category", { id });
}

export function saveGradeItem(args: {
  categoryId: number;
  id?: number;
  name: string;
  score: number;
  maxScore: number;
}): Promise<void> {
  return invoke("save_grade_item", args);
}

export function deleteGradeItem(id: number): Promise<void> {
  return invoke("delete_grade_item", { id });
}

/** 86.6667 -> "86.7%"; whole numbers drop the decimal ("90%"). */
export function formatPercent(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return `${Number.isInteger(rounded) ? rounded : rounded.toFixed(1)}%`;
}

/** "9/10" — JS number formatting already drops trailing zeros ("8.5/10"). */
export function formatScore(score: number, maxScore: number): string {
  return `${score}/${maxScore}`;
}
