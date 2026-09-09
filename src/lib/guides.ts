import { invoke } from "@tauri-apps/api/core";

import { formatStamp, todayIso } from "@/lib/schedule";

/** Mirrors the backend MASTER_OUTPUT constant (guides.rs). */
export const MASTER_OUTPUT_PATH = "Study Guides/Semester Master.html";
/** Mirrors `db.rs::SESSION_SCOPE_PREFIX` — a session scope names its transcript. */
export const SESSION_SCOPE_PREFIX = "session:";
/** Mirrors `db.rs::UNIT_SCOPE_PREFIX` — a unit scope names the division by its row id. */
export const UNIT_SCOPE_PREFIX = "unit:";
/** The small documents' scopes (SPEC §8.6), mirroring `db.rs`. */
export const BRIEF_SCOPE_PREFIX = "brief:";
export const PROJECT_SCOPE = "project";
export const PREREAD_SCOPE_PREFIX = "preread:";
export const KIT_SCOPE_PREFIX = "kit:";

export function briefScope(deadlineId: number): string {
  return `${BRIEF_SCOPE_PREFIX}${deadlineId}`;
}

/** The deadline kinds a brief is written for, mirroring `briefs::BRIEFABLE_KINDS`. */
export const BRIEFABLE_KINDS: readonly string[] = ["assignment", "project"];

export function kitScope(relPath: string): string {
  return `${KIT_SCOPE_PREFIX}${relPath}`;
}

/** Which family of document a `guides` row is (`db.rs::scope_family`). */
export type GuideFamily =
  | "guide"
  | "session"
  | "practice"
  | "brief"
  | "project"
  | "preread"
  | "kit";

/** What the viewer's eyebrow calls a document of each family. */
export const FAMILY_LABELS: Record<GuideFamily, string> = {
  guide: "Study guide",
  session: "Session",
  practice: "Practice exam",
  brief: "Homework brief",
  project: "Project workbook",
  preread: "Pre-read",
  kit: "Presentation kit",
};

/**
 * The `guides.scope` for one of the course's own divisions (SPEC §8.1): its
 * id, so a division a rescan renames keeps its guide. What a scope is called
 * on screen comes from the backend with the guide (`label`) or the job
 * (`scopeLabel`), since the key itself is a number the reader never sees.
 */
export function unitScope(unitId: number): string {
  return `${UNIT_SCOPE_PREFIX}${unitId}`;
}

export interface GuideInfo {
  scope: string; // folder rel path | 'master' | 'unit:<id>' | 'session:<path>' | 'practice:<path>'
  /** What the scope is called on screen — the division's name for a unit scope. */
  label: string;
  relPath: string; // e.g. "Study Guides/Module 1.html"
  generatedAt: number; // unix seconds, set when the job succeeded
  stale: boolean; // source manifest no longer matches the files on disk
  /** What changed since it was written (SPEC §7 step 5): the names behind `stale`. */
  diff: ManifestDiff;
  /** A guide, or one of the documents that share the table to inherit the
   *  viewer and staleness and are listed apart (SPEC §8.4, §8.3, §8.6). */
  family: GuideFamily;
}

/** Mirrors `extract.rs::ManifestDiff`. */
export interface ManifestDiff {
  added: string[];
  changed: string[];
  removed: string[];
  /** The stored manifest could not be read: stale whatever the lists hold. */
  unreadable: boolean;
}

/**
 * The row's phrase for a stale guide: `2 files added, 1 changed, 1 removed` —
 * the noun once, on the first count; `sources changed` when nothing is named.
 */
export function deltaLabel(diff: ManifestDiff): string {
  const parts: string[] = [];
  for (const [list, verb] of [
    [diff.added, "added"],
    [diff.changed, "changed"],
    [diff.removed, "removed"],
  ] as const) {
    if (list.length === 0) continue;
    parts.push(
      parts.length === 0
        ? `${list.length} ${list.length === 1 ? "file" : "files"} ${verb}`
        : `${list.length} ${verb}`,
    );
  }
  return parts.length === 0 ? "sources changed" : parts.join(", ");
}

/** The names behind the phrase, one per line, for a tooltip. */
export function deltaTitle(diff: ManifestDiff): string {
  const name = (path: string) => path.split("/").pop() ?? path;
  const lines = [
    ...diff.added.map((p) => `added · ${name(p)}`),
    ...diff.changed.map((p) => `changed · ${name(p)}`),
    ...diff.removed.map((p) => `removed · ${name(p)}`),
  ];
  return lines.length === 0
    ? "Sources changed since this guide was written"
    : `Since this guide was written:\n${lines.join("\n")}`;
}

/** Guides for a class, staleness computed on demand backend-side. */
export function listGuides(classId: number): Promise<GuideInfo[]> {
  return invoke<GuideInfo[]>("list_guides", { classId });
}

/** Guide HTML for the sandboxed in-app viewer. */
export function readGuide(classId: number, scope: string): Promise<string> {
  return invoke<string>("read_guide", { classId, scope });
}

/** Display-only footer stamp baked into the prompt at enqueue time. */
function generatedAtLabel(): string {
  return new Date()
    .toLocaleString("en-US", {
      month: "long",
      day: "numeric",
      year: "numeric",
      hour: "numeric",
      minute: "2-digit",
    })
    .toUpperCase();
}

/**
 * Enqueues a module_guide job (manual trigger only — synthesis is never
 * automatic). The label becomes the guide footer's generated-at stamp; the
 * guides row's own timestamp is set backend-side at completion.
 */
export function synthesizeModule(
  classId: number,
  moduleRelPath: string,
): Promise<number> {
  return invoke<number>("synthesize_module", {
    classId,
    moduleRelPath,
    generatedAtLabel: generatedAtLabel(),
  });
}

/**
 * Enqueues a guide for one of the course's own divisions (SPEC §8.1) — the
 * guide a filed lecture reaches through its corpus note (SPEC §8.5).
 */
export function synthesizeUnit(
  classId: number,
  unitId: number,
): Promise<number> {
  return invoke<number>("synthesize_unit", {
    classId,
    unitId,
    generatedAtLabel: generatedAtLabel(),
  });
}

/** Enqueues the exclusive master_guide job (SPEC §8.2, manual trigger only). */
export function synthesizeMaster(classId: number): Promise<number> {
  return invoke<number>("synthesize_master", {
    classId,
    generatedAtLabel: generatedAtLabel(),
  });
}

/**
 * Enqueues a practice exam (SPEC §8.3) for a folder rel path, a division's
 * `unitScope`, or "master". Same duplicate-active guard as the guides; the
 * exam lands in the workspace's practice list when the job succeeds.
 */
export function generatePractice(
  classId: number,
  scope: string,
  focus: string | null = null,
): Promise<number> {
  const trimmed = focus?.trim() ?? "";
  return invoke<number>("generate_practice", {
    classId,
    scope,
    generatedAtLabel: generatedAtLabel(),
    // YYYY-MM-DD in local time — names the file, and dates the rubric's next
    // assessment.
    dateLabel: todayIso(),
    focus: trimmed === "" ? null : trimmed,
  });
}

/** Re-invokes a failed master run with --resume <session_id> (SPEC §6). */
export function resumeMasterGuide(jobId: number): Promise<number> {
  return invoke<number>("resume_master_guide", { jobId });
}

export function formatGeneratedAt(unixSec: number): string {
  return formatStamp(new Date(unixSec * 1000));
}

// --- The small documents (SPEC §8.6) ------------------------------------------

/** `Write the brief` on a deadline row: maps the assignment to its window. */
export function writeBrief(classId: number, deadlineId: number): Promise<number> {
  return invoke<number>("write_brief", {
    classId,
    deadlineId,
    generatedAtLabel: generatedAtLabel(),
  });
}

/** `Write the workbook` in the Project section. */
export function writeWorkbook(classId: number): Promise<number> {
  return invoke<number>("write_workbook", {
    classId,
    generatedAtLabel: generatedAtLabel(),
  });
}

/** `Presentation kit` on a paper's row. */
export function writePresentationKit(classId: number, relPath: string): Promise<number> {
  return invoke<number>("write_presentation_kit", {
    classId,
    relPath,
    generatedAtLabel: generatedAtLabel(),
  });
}

/** `Write the pre-read` on a coming week's row in Lectures. */
export function writePreread(classId: number, unitId: number, week: number): Promise<number> {
  return invoke<number>("write_preread", {
    classId,
    unitId,
    week,
    generatedAtLabel: generatedAtLabel(),
  });
}

/** One of the project's items, as the section reads it. */
export interface ProjectItem {
  id: number;
  title: string;
  kind: string;
  dueAt: string;
  status: "open" | "done";
  notes: string | null;
  description: string | null;
  fromCanvas: boolean;
}

/** What the Project section shows; null for a class with no project item. */
export interface ProjectStatus {
  items: ProjectItem[];
  next: ProjectItem | null;
  /** The file the `Project material` picker points at. */
  material: string | null;
  /** The project category's weight, summed over the categories named for it. */
  weight: number;
}

export function projectStatus(classId: number): Promise<ProjectStatus | null> {
  return invoke<ProjectStatus | null>("project_status", { classId, today: todayIso() });
}

/** Points the picker at a class file, or clears it with null. */
export function setProjectMaterial(classId: number, relPath: string | null): Promise<void> {
  return invoke("set_project_material", { classId, relPath });
}

/** A coming week a pre-read can be written for, or one already written. */
export interface PrereadInfo {
  unitId: number;
  unitName: string;
  week: number;
  /** The meeting's date, `YYYY-MM-DD`; null for a written pre-read whose week lost its date. */
  meetsOn: string | null;
  scope: string;
  files: number;
  /** Whether the week still qualifies: material filed, no transcript, the meeting coming. */
  candidate: boolean;
  relPath: string | null;
  generatedAt: number | null;
  stale: boolean;
}

export function listPrereads(classId: number): Promise<PrereadInfo[]> {
  return invoke<PrereadInfo[]>("list_prereads", { classId, today: todayIso() });
}

/** A note dated for a distilled session, and whether it carries `Against the room`. */
export interface ReviewTarget {
  relPath: string;
  name: string;
  date: string;
  sessionRelPath: string;
  transcriptRelPath: string;
  unitName: string;
  reviewed: boolean;
}

export function listNoteReviews(classId: number): Promise<ReviewTarget[]> {
  return invoke<ReviewTarget[]>("list_note_reviews", { classId });
}

/** `Against the room` on a note's row: the read-only review, appended by the app. */
export function reviewNote(classId: number, relPath: string): Promise<number> {
  return invoke<number>("review_note", { classId, relPath });
}
