import { invoke } from "@tauri-apps/api/core";

/** One flagged item (SPEC §8.4), with the session it came from. */
export interface HintInfo {
  id: number;
  kind: string; // emphasis | exam_hint | correction | confusion | action | thread
  text: string;
  /** An `HH:MM` the transcript's own headings resolve; null on an untimed one. */
  anchor: string | null;
  unitId: number;
  unitName: string;
  /** The transcript, class-relative — what the anchor opens. */
  relPath: string;
  /** The session's date off the transcript's name, YYYY-MM-DD; "" when it carries none. */
  date: string;
  /** What the session was about — the digest's title; "" until it has been read. */
  title: string;
}

/** Every flagged item of a class, newest session first. */
export function listHints(classId: number): Promise<HintInfo[]> {
  return invoke<HintInfo[]>("list_hints", { classId });
}

/** Mirrors `lectures::hint_kind_label` — the chip's words. */
export function hintKindLabel(kind: string): string {
  switch (kind) {
    case "emphasis":
      return "Emphasis";
    case "exam_hint":
      return "Exam hint";
    case "correction":
      return "Correction";
    case "confusion":
      return "Where the room got stuck";
    case "action":
      return "Action";
    case "thread":
      return "Builds on";
    default:
      return "Flagged";
  }
}

/** The id the document register gives a transcript's `## HH:MM` heading. */
export function anchorId(anchor: string): string {
  return `t-${anchor.replace(":", "-")}`;
}
