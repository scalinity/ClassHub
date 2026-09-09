import { invoke } from "@tauri-apps/api/core";

import type { AnnouncementAction } from "@/lib/canvas";
import { todayIso } from "@/lib/schedule";

/**
 * SPEC §12 — Today: what the app knows about the day, from the tables and
 * nothing else. What is due rides the deadlines query the strip reads.
 */

export interface TodayMeeting {
  classId: number;
  className: string;
  classColor: string;
  startTime: string;
  endTime: string;
  /** Where the course is today (SPEC §8.5), and the week where the reading
   *  is a filed lecture's. */
  unitName: string | null;
  week: number | null;
  /** A pre-read written for today's meeting, by its scope. */
  prereadScope: string | null;
}

export interface UndoGroup {
  text: string;
  auditIds: number[];
}

export interface Overnight {
  night: string;
  summary: string;
  finishedAt: number | null;
  stoppedBy: string | null;
  running: boolean;
  /** One `Undo` per batch the run wrote that nothing has reversed. */
  undo: UndoGroup[];
}

export interface TodayNotice {
  id: number;
  classId: number;
  className: string;
  classColor: string;
  title: string;
  postedAt: string;
  actions: AnnouncementAction[];
}

export interface Waiting {
  classId: number | null;
  className: string | null;
  classColor: string | null;
  kind: "cards" | "deadlines" | "recordings" | "transcript" | "signin";
  text: string;
}

export interface TodaySummary {
  meetings: TodayMeeting[];
  overnight: Overnight | null;
  /** Unix seconds of the open before this one; the notices are since it. */
  since: number | null;
  notices: TodayNotice[];
  waiting: Waiting[];
}

export function todaySummary(): Promise<TodaySummary> {
  return invoke<TodaySummary>("today_summary", { today: todayIso() });
}

/** The one query the block reads, keyed by the day so a dashboard left open
 *  across midnight asks again. */
export function todayQuery() {
  return {
    queryKey: ["today", todayIso()] as const,
    queryFn: todaySummary,
  };
}
