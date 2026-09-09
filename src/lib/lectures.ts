import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

import type { TreeNode } from "@/lib/materials";
import { queryClient } from "@/lib/query";

/** Mirrors transcribe.rs MEDIA_EXTS — what can be handed to Parakeet. */
const MEDIA_EXTS = [
  "m4a", "mp3", "wav", "aac", "flac", "ogg", "opus",
  "mp4", "m4v", "mov", "webm",
];
/** Caption tracks Zoom hands out, plus its in-meeting "Save Transcript" txt. */
const CAPTION_EXTS = ["vtt", "srt", "txt"];

export type SourceKind = "link" | "caption" | "media" | "unknown";

/** What the form is looking at, so it can say what will happen before it does. */
export function classifySource(source: string): SourceKind {
  const trimmed = source.trim();
  if (!trimmed) return "unknown";
  if (/^https?:\/\//i.test(trimmed)) return "link";
  const ext = trimmed.split(".").pop()?.toLowerCase() ?? "";
  if (MEDIA_EXTS.includes(ext)) return "media";
  if (CAPTION_EXTS.includes(ext)) return "caption";
  return "unknown";
}

export interface AddLectureRequest {
  classId: number;
  source: string;
  /** The week from the course's own schedule; null routes through the inbox. */
  week: number | null;
  date: string;
  title: string | null;
  digest: boolean;
  /** The found recording this lecture is, whose row the run marks filed. */
  recordingId?: number;
}

export interface AddResult {
  relPath: string;
  routedToInbox: boolean;
  /** The course's own name for the division this lecture now feeds. */
  unitName: string | null;
  speakers: string[];
  digestJobId: number | null;
  /** Present when a digest was asked for and did not start. */
  digestError?: string;
}

export interface LectureProgress {
  classId: number;
  stage: string;
  done: boolean;
  result?: AddResult;
  error?: string;
}

// --- External store (push-based Tauri events; no useEffect per workspace rules) ---
//
// Ingestion outlives the dialog: transcribing a lecture runs for minutes, so
// progress lives here rather than in component state and survives the form
// being closed and reopened.

let snapshot: ReadonlyMap<number, LectureProgress> = new Map();
const listeners = new Set<() => void>();

function emit(next: ReadonlyMap<number, LectureProgress>) {
  snapshot = next;
  for (const notify of listeners) notify();
}

let initialized = false;
function init() {
  if (initialized) return;
  initialized = true;
  void listen<LectureProgress>("lecture://progress", (e) => {
    const next = new Map(snapshot);
    next.set(e.payload.classId, e.payload);
    emit(next);
    if (e.payload.done && e.payload.result) {
      // The transcript is on disk, indexed and mapped to its division by the
      // time this fires, so the material tree, the digest listing and the
      // lecture map all have something new to show.
      void queryClient.invalidateQueries({ queryKey: ["classTree"] });
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
      void queryClient.invalidateQueries({ queryKey: ["contributions"] });
      // A run opened from a found recording marked its row on the way.
      void queryClient.invalidateQueries({ queryKey: ["recordings"] });
    }
  });
}
init();

export function useLectureProgress(classId: number): LectureProgress | null {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => snapshot.get(classId) ?? null,
  );
}

/** Clears a finished run so the form starts clean next time it opens. */
export function clearLectureProgress(classId: number) {
  if (!snapshot.has(classId)) return;
  const next = new Map(snapshot);
  next.delete(classId);
  emit(next);
}

// --- Actions ---

/** Fire-and-forget: the outcome arrives on the progress event, not here. */
export function addLecture(request: AddLectureRequest): Promise<void> {
  return invoke("add_lecture", { request });
}

export function digestLecture(
  classId: number,
  relPath: string,
  date: string,
): Promise<number> {
  return invoke<number>("digest_lecture", { classId, relPath, date });
}

// --- Helpers ---

/** Pulls a session date out of a Zoom filename, which is where it usually is:
 *  `GMT20260824-210000_Recording.m4a`, or a plain `2026-08-24` anywhere in it. */
export function dateFromFileName(source: string): string | null {
  const name = source.split("/").pop() ?? source;
  const gmt = /(?:^|[^0-9])(\d{4})(\d{2})(\d{2})(?:[^0-9]|$)/.exec(name);
  const iso = /(\d{4})-(\d{2})-(\d{2})/.exec(name);
  const match = iso ?? gmt;
  if (!match) return null;
  const [, year, month, day] = match;
  if (Number(month) < 1 || Number(month) > 12) return null;
  if (Number(day) < 1 || Number(day) > 31) return null;
  return `${year}-${month}-${day}`;
}

/** Mirrors db.rs WEEKS_DIR — where a lecture lives (SPEC §4). */
export const WEEKS_DIR = "Weeks";

/** One week a lecture can be filed into, and the division it would feed. */
export interface WeekSlot {
  week: number;
  /** The folder it files into, as the backend builds it. */
  folder: string;
  unitId: number;
  unitName: string;
  /** `week` where the division is the week itself; else the course's word
   *  for the division that groups it, whose name the option then carries. */
  unitKind: string;
  /** The date the course published for this week, where it published one. */
  meetsOn: string | null;
}

export interface LectureWeeks {
  slots: WeekSlot[];
  /** The week nearest the session date; null when the course published no
   *  dates to measure against, and the form has to ask outright. */
  defaultWeek: number | null;
}

/**
 * The weeks this course declares, and which one a session on `date` lands on.
 *
 * Both are resolved backend-side from the course's own schedule rather than
 * computed here: weeks are not uniformly spaced (SPEC §1), so nothing may
 * divide a date by seven to get one.
 */
export function lectureWeeks(
  classId: number,
  date: string,
): Promise<LectureWeeks> {
  return invoke<LectureWeeks>("lecture_weeks", { classId, date });
}

/** One lecture's place in the course's structure (SPEC §8.5). */
export interface Contribution {
  relPath: string;
  unitId: number;
  unitName: string;
  corpusRelPath: string;
  /** Whether the distilled note behind the map has actually been written. */
  distilled: boolean;
  /** Whether a digest has read the session for what was flagged (SPEC §8.4)
   *  — false for one distilled before the ledger existed, whatever it found. */
  hintsRead: boolean;
  summary: string;
}

export function listLectureContributions(
  classId: number,
): Promise<Contribution[]> {
  return invoke<Contribution[]>("list_lecture_contributions", { classId });
}

export interface FiledTranscript {
  name: string;
  relPath: string;
}

/**
 * Every transcript filed under `Weeks/`.
 *
 * A transcript routed through the inbox is filed by approving a move, which
 * never goes near the ingestion flow — so without this, a sorted transcript
 * would have no way left to be distilled.
 */
export function collectTranscripts(
  nodes: TreeNode[] | undefined,
): FiledTranscript[] {
  const found: FiledTranscript[] = [];
  const walk = (list: TreeNode[]) => {
    for (const node of list) {
      if (node.dir) {
        walk(node.children ?? []);
      } else if (
        node.relPath.startsWith(`${WEEKS_DIR}/`) &&
        node.relPath.toLowerCase().endsWith(".md")
      ) {
        found.push({ name: node.name, relPath: node.relPath });
      }
    }
  };
  walk(nodes ?? []);
  return found.sort((a, b) => b.relPath.localeCompare(a.relPath));
}

// --- Recordings found behind the Zoom tool (SPEC §7.1) -----------------------

/** One recording the sync listed and nothing has captured yet. */
export interface RecordingInfo {
  id: number;
  classId: number;
  title: string;
  /** Local ISO, YYYY-MM-DDTHH:MM. */
  recordedAt: string;
  durationMinutes: number;
  /** `new` waits for a capture or the form; `failed` says why in `note`. */
  status: "new" | "failed";
  note: string | null;
}

export function listRecordings(classId: number): Promise<RecordingInfo[]> {
  return invoke<RecordingInfo[]>("list_recordings", { classId });
}

/** `Find recordings`: lists the course's recordings through Canvas and
 *  captures what waits, hidden. The outcome arrives on the progress event. */
export function findRecordings(classId: number): Promise<void> {
  return invoke("find_recordings", { classId });
}

/** The player link of a waiting recording, for the form to open pre-filled. */
export function recordingPlayUrl(id: number): Promise<string | null> {
  return invoke<string | null>("recording_play_url", { id });
}

/** `3 h 29 m`, `45 m`. */
export function formatDuration(minutes: number): string {
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  if (h === 0) return `${m} m`;
  return m === 0 ? `${h} h` : `${h} h ${m} m`;
}

export interface FindProgress {
  classId: number;
  stage: string;
  done: boolean;
  summary?: string;
  error?: string;
}

let findSnapshot: ReadonlyMap<number, FindProgress> = new Map();
const findListeners = new Set<() => void>();

function emitFind(next: ReadonlyMap<number, FindProgress>) {
  findSnapshot = next;
  for (const notify of findListeners) notify();
}

let findInitialized = false;
function initFind() {
  if (findInitialized) return;
  findInitialized = true;
  void listen<FindProgress>("recordings://progress", (e) => {
    const next = new Map(findSnapshot);
    next.set(e.payload.classId, e.payload);
    emitFind(next);
    if (e.payload.done) {
      void queryClient.invalidateQueries({ queryKey: ["recordings"] });
      void queryClient.invalidateQueries({ queryKey: ["classTree"] });
      void queryClient.invalidateQueries({ queryKey: ["contributions"] });
    }
  });
}
initFind();

export function useFindProgress(classId: number): FindProgress | null {
  return useSyncExternalStore(
    (cb) => {
      findListeners.add(cb);
      return () => findListeners.delete(cb);
    },
    () => findSnapshot.get(classId) ?? null,
  );
}

/** Clears a finished find so the section's line goes with it. */
export function clearFindProgress(classId: number) {
  if (!findSnapshot.has(classId)) return;
  const next = new Map(findSnapshot);
  next.delete(classId);
  emitFind(next);
}

/** How the Add lecture form opens: empty, or pre-filled from a recording
 *  the sync found whose week is the form's to pick. */
export interface LectureFormOpen {
  source?: string;
  date?: string;
  recordingId?: number;
}
