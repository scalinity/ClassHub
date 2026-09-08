import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

import { queryClient } from "@/lib/query";
import { daysUntil, formatStamp, todayIso } from "@/lib/schedule";

/**
 * SPEC §7.2 — reading course structure, files and assignments from Canvas.
 *
 * There is deliberately no "connected" state here and no key to manage. UF has
 * closed both credentialed paths, so the app reads through a signed-in Canvas
 * window and keeps that session's cookie in the Keychain, which is what lets a
 * relaunch skip the sign-in. Holding it is not the same as Canvas still
 * honouring it, so a sync either finds the session live or asks for a sign-in,
 * and the second is an ordinary step rather than an error to recover from.
 */

/** The course's own division, in the course's own words (SPEC §5). */
export interface Unit {
  id: number;
  ordinal: number;
  kind: "module" | "week" | "part";
  name: string;
  /** The course's own number for it, read from its label (`Week 7` is 7);
   *  null for a row named without one. */
  number: number | null;
  relPath: string | null;
  startsOn: string | null;
  endsOn: string | null;
  /** The weeks it spans where the course said so — a Part's `(Weeks 1-8)`. */
  firstWeek: number | null;
  lastWeek: number | null;
  source: UnitSource;
  /** Files a guide would read beside the division's distilled lectures —
   *  under its folder and its week folders (SPEC §8.5). Null on a row
   *  nothing counted for. */
  materials: number | null;
}

/** Which reader supplied the division. A folder is not one of them: it is where
 *  material sits, not something the course declared. */
export type UnitSource = "canvas" | "syllabus";

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
  deadlinesRecorded: number;
  /** Open deadlines closed because Canvas holds a submission for them. */
  deadlinesCompleted: number;
  filesStaged: number;
  /** Staged files the sync filed on the spot, where Canvas or the name placed them (SPEC §7.2). */
  filesFiled: number;
  /** Grade items written or updated from graded, posted submissions. */
  gradesRecorded: number;
  /** Announcements recorded or updated — what NOTICES gained. */
  announcementsRecorded: number;
  /** Canvas Pages and the syllabus page written into the extract cache. */
  pagesWritten: number;
  notes: string[];
  error: string | null;
}

export interface SyncProgress {
  stage: string;
  done: boolean;
  /** Started by the launch rather than by a press (SPEC §7.2). */
  launch: boolean;
  /** Ended because Canvas wants a sign-in the sync could not ask for — the
   *  one failure that is a note rather than a stopped sync. */
  signInNeeded: boolean;
  results?: ClassOutcome[];
  error?: string;
}

/** One Canvas announcement — what the professor said, stripped to text.
 *  A record rather than a queue: nothing here is unread or waiting. */
export interface Announcement {
  id: number;
  canvasId: string;
  title: string;
  body: string;
  /** Local ISO, YYYY-MM-DDTHH:MM — the deadline shape, so the same labels apply. */
  postedAt: string;
}

export function listUnits(classId: number): Promise<Unit[]> {
  return invoke<Unit[]>("list_units", { classId });
}

export function getCanvasStatus(): Promise<CanvasStatus> {
  return invoke<CanvasStatus>("canvas_status");
}

/** The class's announcements, newest first. */
export function listAnnouncements(classId: number): Promise<Announcement[]> {
  return invoke<Announcement[]>("list_announcements", { classId });
}

/** The class-relative path of the mirrored Canvas syllabus page, or null
 *  until a sync has written one. */
export function canvasSyllabus(classId: number): Promise<string | null> {
  return invoke<string | null>("canvas_syllabus", { classId });
}

/** Starts a sync. Rejects only when one could not be started — one already
 *  running — since everything after that arrives on the progress event.
 *  An empty `classIds` syncs every class.
 *
 *  The running state is set here rather than waited for. The first event is a
 *  round trip away, and a button that still reads SYNC CANVAS after being
 *  pressed invites a second press. Setting it also clears the previous run's
 *  report, which stops being current the moment this one starts. */
export function syncCanvas(classIds: number[] = []): Promise<void> {
  // Never published over a run already in flight. The backend refuses this
  // call, and clearing on that refusal would take the running sync's own
  // progress with it.
  if (snapshot !== null && !snapshot.done) {
    return invoke<void>("sync_canvas", { classIds });
  }
  publish({ stage: "Starting…", done: false, launch: false, signInNeeded: false });
  return invoke<void>("sync_canvas", { classIds }).catch((e: unknown) => {
    publish(null);
    throw e;
  });
}

// --- External store (push-based Tauri events; no useEffect per workspace rules) ---
//
// A sync outlives whichever screen started it: it waits on a human completing
// SSO, which can take minutes, so progress lives here rather than in component
// state. One sync runs at a time, so one slot is enough.

let snapshot: SyncProgress | null = null;
const listeners = new Set<() => void>();

function publish(next: SyncProgress | null) {
  snapshot = next;
  for (const notify of listeners) notify();
}

let initialized = false;
function init() {
  if (initialized) return;
  initialized = true;
  listen<SyncProgress>("canvas://progress", (e) => {
    publish(e.payload);
    if (e.payload.done) {
      // By the time this fires the units, proposals and downloads are all
      // committed, so every surface a sync touches has something new to show.
      void queryClient.invalidateQueries({ queryKey: ["units"] });
      void queryClient.invalidateQueries({ queryKey: ["canvasStatus"] });
      void queryClient.invalidateQueries({ queryKey: ["deadlineProposals"] });
      void queryClient.invalidateQueries({ queryKey: ["deadlines"] });
      void queryClient.invalidateQueries({ queryKey: ["grades"] });
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      void queryClient.invalidateQueries({ queryKey: ["announcements"] });
      void queryClient.invalidateQueries({ queryKey: ["canvasSyllabus"] });
    }
    // Registration failing is not something a screen can recover from — every
    // later sync would appear to hang with nothing said anywhere — so it is at
    // least worth a line in the console rather than a discarded promise.
  }).catch((e: unknown) => console.error("canvas progress listener failed", e));
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

// --- Display helpers ---

export function formatSyncedAt(seconds: number | null): string {
  if (seconds === null) return "Never";
  return formatStamp(new Date(seconds * 1000));
}

/** Past this many calendar days the dashboard's sync line turns destructive. */
export const SYNC_STALE_DAYS = 7;

/** Whole calendar days since the sync, the way deadlines count days: a sync
 *  late last night is a day ago this morning. */
export function daysSinceSync(seconds: number): number {
  return -daysUntil(todayIso(new Date(seconds * 1000)));
}

/** The dashboard's line (SPEC §12), after "Canvas": `synced 6 days ago`,
 *  `synced today`, `never synced`, or `syncing…` while one runs. */
export function syncAgeLabel(seconds: number | null, running: boolean): string {
  if (running) return "syncing…";
  if (seconds === null) return "never synced";
  const days = daysSinceSync(seconds);
  if (days <= 0) return "synced today";
  if (days === 1) return "synced yesterday";
  return `synced ${days} days ago`;
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
  if (outcome.deadlinesRecorded > 0) {
    parts.push(
      `${outcome.deadlinesRecorded} deadline${outcome.deadlinesRecorded === 1 ? "" : "s"} recorded`,
    );
  }
  if (outcome.deadlinesCompleted > 0) {
    parts.push(
      `${outcome.deadlinesCompleted} deadline${outcome.deadlinesCompleted === 1 ? "" : "s"} done`,
    );
  }
  if (outcome.gradesRecorded > 0) {
    parts.push(
      `${outcome.gradesRecorded} grade${outcome.gradesRecorded === 1 ? "" : "s"} recorded`,
    );
  }
  if (outcome.announcementsRecorded > 0) {
    parts.push(
      `${outcome.announcementsRecorded} notice${outcome.announcementsRecorded === 1 ? "" : "s"}`,
    );
  }
  if (outcome.pagesWritten > 0) {
    parts.push(
      `${outcome.pagesWritten} Canvas page${outcome.pagesWritten === 1 ? "" : "s"} mirrored`,
    );
  }
  if (outcome.filesFiled > 0) {
    parts.push(
      `${outcome.filesFiled} file${outcome.filesFiled === 1 ? "" : "s"} filed`,
    );
  }
  const waiting = outcome.filesStaged - outcome.filesFiled;
  if (waiting > 0) {
    parts.push(`${waiting} file${waiting === 1 ? "" : "s"} in the inbox`);
  }
  return parts.length > 0 ? parts.join(" · ") : "nothing new";
}
