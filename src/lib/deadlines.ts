import { invoke } from "@tauri-apps/api/core";

import { todayIso } from "./schedule";

export const DEADLINE_KINDS = [
  "assignment",
  "exam",
  "quiz",
  "project",
  "other",
] as const;
export type DeadlineKind = (typeof DEADLINE_KINDS)[number];

export interface Deadline {
  id: number;
  classId: number;
  className: string;
  classColor: string;
  title: string;
  /** Every write path validates against DEADLINE_KINDS, so rows are in-domain. */
  kind: DeadlineKind;
  /** ISO as stored: YYYY-MM-DD, optionally with THH:MM[:SS]. */
  dueAt: string;
  notes: string | null;
  status: "open" | "done";
  source: DeadlineSource;
  /** Set when the Canvas sync tracks this row: its due date follows Canvas
   *  and a submission marks it done, whoever first put it on the list. */
  canvasAssignmentId: string | null;
  /** The assignment's own description from Canvas, as text; refreshed by
   *  every sync on a tracked row (SPEC §7.2). */
  description: string | null;
}

/** Who put this on the list: added by hand, by chat, by a syllabus scan, or
 *  read from Canvas. Approval stamps a proposal's own source onto the
 *  deadline, so a due date that turns out wrong can be traced back. */
export type DeadlineSource =
  | "manual"
  | "agent"
  | "syllabus"
  | "canvas"
  | "announcement";

/** The badge a non-manual deadline or proposal carries. */
const DEADLINE_SOURCE_COPY: Record<
  Exclude<DeadlineSource, "manual">,
  { label: string; title: string }
> = {
  syllabus: { label: "from the syllabus", title: "Read from a syllabus scan" },
  canvas: { label: "from Canvas", title: "Read from Canvas, with its own due date" },
  agent: { label: "from chat", title: "Added in chat" },
  announcement: {
    label: "from a notice",
    title: "Read out of a Canvas announcement by the announcement scan",
  },
};

/** The badge for a deadline the Canvas sync tracks — the same words as the
 *  grade item's tag, because the same thing is true: an edit here lasts
 *  until the next sync. It outranks the source, since a syllabus row Canvas
 *  has claimed is Canvas's now. */
const CANVAS_TRACKED_COPY = {
  label: "from Canvas",
  title:
    "Tracked from Canvas — its due date follows the next sync, and handing the assignment in marks it done",
};

/** The badge for a source, or null where there is nothing to say.
 *
 *  `manual` earns no badge — the reader added it and knows. A source this
 *  build does not recognize earns none either: indexing the record directly
 *  threw on the unknown key and took the whole row's render down with it, and
 *  a row that renders without a chip is better than a list that does not
 *  render at all. A row Canvas tracks says so whatever its source. */
export function deadlineSourceBadge(
  source: DeadlineSource,
  canvasAssignmentId: string | null = null,
): { label: string; title: string } | null {
  if (canvasAssignmentId !== null) return CANVAS_TRACKED_COPY;
  return source === "manual" ? null : (DEADLINE_SOURCE_COPY[source] ?? null);
}

/** Every deadline across every class, due-soonest first — one query for the
 * dashboard strip, the class list, and anything else that filters it. */
export function listDeadlines(): Promise<Deadline[]> {
  return invoke<Deadline[]>("list_deadlines");
}

/** Create (no id) or amend a deadline; validation matches the chat tool's. */
export function saveDeadline(args: {
  classId: number;
  id?: number;
  title: string;
  kind: DeadlineKind;
  dueAt: string;
  notes?: string;
}): Promise<void> {
  return invoke("save_deadline", {
    classId: args.classId,
    id: args.id ?? null,
    title: args.title,
    kind: args.kind,
    dueAt: args.dueAt,
    notes: args.notes ?? null,
  });
}

export function setDeadlineStatus(id: number, done: boolean): Promise<void> {
  return invoke("set_deadline_status", { id, done });
}

/** The deleted row is kept in the audit log — recoverable, so no confirm step. */
export function deleteDeadline(id: number): Promise<void> {
  return invoke("delete_deadline", { id });
}

// --- Syllabus scan (SPEC §11): job → proposals → confirm → insert -----------

export interface DeadlineProposal {
  id: number;
  classId: number;
  title: string;
  /** Normalized into DEADLINE_KINDS by the scan's finalize. */
  kind: DeadlineKind;
  dueAt: string;
  notes: string | null;
  createdAt: number;
  /** Which reader proposed it — the card says so, because "Canvas says this is
   *  due then" and "a PDF seemed to say so" deserve different scrutiny. */
  source: "syllabus" | "canvas" | "announcement";
}

/** A recurring proposal read as one card (SPEC §11): pending cards sharing a
 *  title stem, a kind, a source and a weekday across three or more dates —
 *  derived when the queue is listed, never stored. */
export interface DeadlineSeries {
  stem: string;
  kind: DeadlineKind;
  source: "syllabus" | "canvas" | "announcement";
  /** `Tuesday`. */
  weekday: string;
  /** The member cards, by date. */
  ids: number[];
  firstDue: string;
  lastDue: string;
}

/** The queue as the workspace lists it: every pending card, and the series
 *  among them. */
export interface DeadlineQueue {
  proposals: DeadlineProposal[];
  series: DeadlineSeries[];
}

export function getDeadlineProposals(classId: number): Promise<DeadlineQueue> {
  return invoke<DeadlineQueue>("get_deadline_proposals", { classId });
}

/** A series card's Skip: every member card dismissed at once; `approved`
 *  holds the ids that left the queue, `skipped` a line per card that could
 *  not (already resolved elsewhere), which stays as it was. */
export function dismissDeadlineProposals(
  proposalIds: number[],
): Promise<BatchOutcome> {
  return invoke<BatchOutcome>("dismiss_deadline_proposals", { proposalIds });
}

/** Scan one class file (relPath) or the whole class folder (null). */
export function runSyllabusScan(
  classId: number,
  relPath: string | null,
): Promise<number> {
  return invoke<number>("run_syllabus_scan", {
    classId,
    relPath,
    today: todayIso(),
  });
}

/** Approve inserts the deadline with source='syllabus'; skip parks the card. */
export function resolveDeadlineProposal(
  proposalId: number,
  approve: boolean,
): Promise<string> {
  return invoke<string>("resolve_deadline_proposal", { proposalId, approve });
}

/** What ADD ALL did: ids now safe to hide, and one line per card it couldn't
 * add (those stay in the queue). */
export interface BatchOutcome {
  approved: number[];
  skipped: string[];
}

/** Approves a batch server-side: one rejection costs its card, never the
 * rest, and the backend pushes hub-changed once for the whole batch. */
export function approveDeadlineProposals(
  proposalIds: number[],
): Promise<BatchOutcome> {
  return invoke<BatchOutcome>("approve_deadline_proposals", { proposalIds });
}
