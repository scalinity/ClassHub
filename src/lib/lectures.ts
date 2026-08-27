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
