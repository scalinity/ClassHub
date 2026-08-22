import { invoke } from "@tauri-apps/api/core";

export interface GuideInfo {
  scope: string; // module rel path | 'master'
  relPath: string; // e.g. "Study Guides/Module 1.html"
  generatedAt: number; // unix seconds, set when the job succeeded
  stale: boolean; // source manifest no longer matches the files on disk
}

/** Guides for a class, staleness computed on demand backend-side. */
export function listGuides(classId: number): Promise<GuideInfo[]> {
  return invoke<GuideInfo[]>("list_guides", { classId });
}

/** Guide HTML for the sandboxed in-app viewer. */
export function readGuide(classId: number, scope: string): Promise<string> {
  return invoke<string>("read_guide", { classId, scope });
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
  const generatedAtLabel = new Date()
    .toLocaleString("en-US", {
      month: "long",
      day: "numeric",
      year: "numeric",
      hour: "numeric",
      minute: "2-digit",
    })
    .toUpperCase();
  return invoke<number>("synthesize_module", {
    classId,
    moduleRelPath,
    generatedAtLabel,
  });
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
