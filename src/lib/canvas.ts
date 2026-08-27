import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

import { queryClient } from "@/lib/query";

/**
 * SPEC §7.2 — reading course structure, files and assignments from Canvas.
 *
 * There is deliberately no "connected" state here and no key to manage. UF has
 * closed both credentialed paths, so the app reads through a signed-in Canvas
 * window and stores nothing; its reach expires with that session. A sync
 * therefore either finds the session still open or asks for a sign-in, and the
 * second is an ordinary step rather than an error to recover from.
 */

/** The course's own division, in the course's own words (SPEC §5). */
export interface Unit {
  id: number;
  ordinal: number;
  kind: "module" | "week" | "part";
  name: string;
  relPath: string | null;
  startsOn: string | null;
  endsOn: string | null;
  source: UnitSource;
}

export type UnitSource = "canvas" | "syllabus" | "folder";

export interface CanvasStatus {
  /** Unix seconds of the most recent per-class sync; null if never. */
  lastSyncedAt: number | null;
  classesLinked: number;
  host: string;
}

export interface ClassOutcome {
  classId: number;
  className: string;
  canvasCourse: string | null;
  unitsAdded: number;
  deadlinesProposed: number;
  filesStaged: number;
  notes: string[];
  error: string | null;
}

export interface SyncProgress {
  stage: string;
  done: boolean;
  results?: ClassOutcome[];
  error?: string;
}

export function listUnits(classId: number): Promise<Unit[]> {
  return invoke<Unit[]>("list_units", { classId });
}

export function getCanvasStatus(): Promise<CanvasStatus> {
  return invoke<CanvasStatus>("canvas_status");
}

/** Fire-and-forget: the outcome arrives on the progress event, not here.
 *  An empty `classIds` syncs every class. */
export function syncCanvas(classIds: number[] = []): Promise<void> {
  return invoke("sync_canvas", { classIds });
}

// --- External store (push-based Tauri events; no useEffect per workspace rules) ---
//
// A sync outlives whichever screen started it: it waits on a human completing
// SSO, which can take minutes, so progress lives here rather than in component
// state. One sync runs at a time, so one slot is enough.

let snapshot: SyncProgress | null = null;
const listeners = new Set<() => void>();

let initialized = false;
function init() {
  if (initialized) return;
  initialized = true;
  void listen<SyncProgress>("canvas://progress", (e) => {
    snapshot = e.payload;
    for (const notify of listeners) notify();
    if (e.payload.done) {
      // By the time this fires the units, proposals and downloads are all
      // committed, so every surface a sync touches has something new to show.
      void queryClient.invalidateQueries({ queryKey: ["units"] });
      void queryClient.invalidateQueries({ queryKey: ["canvasStatus"] });
      void queryClient.invalidateQueries({ queryKey: ["syllabusProposals"] });
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
    }
  });
}
init();

export function useCanvasSync(): SyncProgress | null {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => snapshot,
  );
}

/** Clears a finished run so a screen starts clean next time it opens. */
export function clearCanvasSync() {
  if (snapshot === null || !snapshot.done) return;
  snapshot = null;
  for (const notify of listeners) notify();
}

// --- Display helpers ---

export function formatSyncedAt(seconds: number | null): string {
  if (seconds === null) return "NEVER";
  return new Date(seconds * 1000)
    .toLocaleString("en-US", {
      day: "numeric",
      month: "short",
      hour: "numeric",
      minute: "2-digit",
    })
    .toUpperCase();
}

/** One line summarizing what a class's sync brought across. */
export function outcomeSummary(outcome: ClassOutcome): string {
  if (outcome.error) return outcome.error;
  const parts: string[] = [];
  if (outcome.unitsAdded > 0) {
    parts.push(
      `${outcome.unitsAdded} division${outcome.unitsAdded === 1 ? "" : "s"}`,
    );
  }
  if (outcome.deadlinesProposed > 0) {
    parts.push(
      `${outcome.deadlinesProposed} deadline${outcome.deadlinesProposed === 1 ? "" : "s"} to review`,
    );
  }
  if (outcome.filesStaged > 0) {
    parts.push(
      `${outcome.filesStaged} file${outcome.filesStaged === 1 ? "" : "s"} in the inbox`,
    );
  }
  return parts.length > 0 ? parts.join(" · ") : "nothing new";
}
