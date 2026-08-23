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
  source: "manual" | "agent" | "syllabus";
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

export interface SyllabusProposal {
  id: number;
  classId: number;
  title: string;
  /** Normalized into DEADLINE_KINDS by the scan's finalize. */
  kind: DeadlineKind;
  dueAt: string;
  notes: string | null;
  createdAt: number;
}

export function getSyllabusProposals(
  classId: number,
): Promise<SyllabusProposal[]> {
  return invoke<SyllabusProposal[]>("get_syllabus_proposals", { classId });
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
export function resolveSyllabusProposal(
  proposalId: number,
  approve: boolean,
): Promise<string> {
  return invoke<string>("resolve_syllabus_proposal", { proposalId, approve });
}

/** What ADD ALL did: ids now safe to hide, and one line per card it couldn't
 * add (those stay in the queue). */
export interface BatchOutcome {
  approved: number[];
  skipped: string[];
}

/** Approves a batch server-side: one rejection costs its card, never the
 * rest, and the backend pushes hub-changed once for the whole batch. */
export function approveAllProposals(
  proposalIds: number[],
): Promise<BatchOutcome> {
  return invoke<BatchOutcome>("approve_syllabus_proposals", { proposalIds });
}
