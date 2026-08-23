import { invoke } from "@tauri-apps/api/core";

export interface GradeItem {
  id: number;
  name: string;
  score: number;
  maxScore: number;
  /** Chat's optional field; the UI form leaves it untouched. */
  gradedAt: string | null;
}

export interface GradeCategory {
  id: number;
  name: string;
  weight: number;
  /** Points earned across this category's items, when any exist. */
  percent: number | null;
  items: GradeItem[];
}

export interface GradesInfo {
  categories: GradeCategory[];
  weightTotal: number;
  /** SPEC §11 math — null until at least one item is recorded. */
  currentGrade: number | null;
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
