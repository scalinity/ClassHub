import { invoke } from "@tauri-apps/api/core";

import { todayIso } from "@/lib/schedule";

/** Mirrors the backend MASTER_OUTPUT constant (guides.rs). */
export const MASTER_OUTPUT_PATH = "Study Guides/Semester Master.html";
/** Mirrors `db.rs::SESSION_SCOPE_PREFIX` — a session scope names its transcript. */
export const SESSION_SCOPE_PREFIX = "session:";
/** Mirrors `db.rs::UNIT_SCOPE_PREFIX` — a unit scope names the division. */
export const UNIT_SCOPE_PREFIX = "unit:";

/** The `guides.scope` for one of the course's own divisions (SPEC §8.1). */
export function unitScope(unitName: string): string {
  return `${UNIT_SCOPE_PREFIX}${unitName}`;
}

/**
 * What a scope is called on screen.
 *
 * A scope is a storage key, and two of the four shapes read as machinery: the
 * app never shows the reader the word "unit" (SPEC §5), and a session names a
 * file path rather than a session.
 */
export function scopeLabel(scope: string): string {
  if (scope === "master") return "Semester master";
  if (scope.startsWith(UNIT_SCOPE_PREFIX)) {
    return scope.slice(UNIT_SCOPE_PREFIX.length);
  }
  if (scope.startsWith(SESSION_SCOPE_PREFIX)) {
    const path = scope.slice(SESSION_SCOPE_PREFIX.length);
    return (path.split("/").pop() ?? path).replace(/\.md$/i, "");
  }
  return scope;
}

export interface GuideInfo {
  scope: string; // folder rel path | 'master' | 'unit:<name>' | 'session:<path>'
  relPath: string; // e.g. "Study Guides/Module 1.html"
  generatedAt: number; // unix seconds, set when the job succeeded
  stale: boolean; // source manifest no longer matches the files on disk
  /** A lecture's session document rather than a module or semester guide.
   *  Shares the table to inherit the viewer and staleness; listed separately. */
  session: boolean;
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
): Promise<number> {
  return invoke<number>("generate_practice", {
    classId,
    scope,
    generatedAtLabel: generatedAtLabel(),
    // YYYY-MM-DD in local time — names the file.
    dateLabel: todayIso(),
  });
}

/** Re-invokes a failed master run with --resume <session_id> (SPEC §6). */
export function resumeMasterGuide(jobId: number): Promise<number> {
  return invoke<number>("resume_master_guide", { jobId });
}

export function formatGeneratedAt(unixSec: number): string {
  return new Date(unixSec * 1000)
    .toLocaleString("en-US", {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    })
    .toUpperCase();
}
