import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

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
  /** Class-relative module folder; null routes through the inbox instead. */
  moduleRelPath: string | null;
  date: string;
  title: string | null;
  digest: boolean;
}

export interface AddResult {
  relPath: string;
  routedToInbox: boolean;
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
      // The transcript is on disk and indexed by the time this fires, so the
      // material tree and any digest listing have something new to show.
      void queryClient.invalidateQueries({ queryKey: ["classTree"] });
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
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

/** Module folders a transcript can be filed into: top-level folders of the
 *  class tree, which is what the app treats as a module (SPEC §4). */
export function moduleOptions(
  nodes: { name: string; relPath: string; dir: boolean }[] | undefined,
): { name: string; relPath: string }[] {
  return (nodes ?? [])
    .filter((n) => n.dir)
    .map((n) => ({ name: n.name, relPath: n.relPath }));
}

/** Mirrors db.rs TRANSCRIPTS_DIR — the per-module folder transcripts live in. */
export const TRANSCRIPTS_DIR = "Transcripts";

export interface FiledTranscript {
  name: string;
  relPath: string;
}

/**
 * Every transcript filed anywhere in the class tree.
 *
 * A transcript routed through the inbox is filed by approving a move, which
 * never goes near the ingestion flow — so without this, a sorted transcript
 * would have no way left to be distilled.
 */
export function collectTranscripts(
  nodes: TranscriptNode[] | undefined,
): FiledTranscript[] {
  const found: FiledTranscript[] = [];
  const walk = (list: TranscriptNode[]) => {
    for (const node of list) {
      if (node.dir) {
        walk(node.children ?? []);
      } else if (
        node.relPath.includes(`/${TRANSCRIPTS_DIR}/`) &&
        node.relPath.toLowerCase().endsWith(".md")
      ) {
        found.push({ name: node.name, relPath: node.relPath });
      }
    }
  };
  walk(nodes ?? []);
  return found.sort((a, b) => b.relPath.localeCompare(a.relPath));
}

interface TranscriptNode {
  name: string;
  relPath: string;
  dir: boolean;
  children?: TranscriptNode[];
}
