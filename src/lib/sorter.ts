import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useSyncExternalStore } from "react";

export interface InboxFile {
  name: string;
  size: number;
  /** Its proposal was dismissed ("leave in inbox") — a decision already made:
   * it stops counting toward the badge and only a manual SORT INBOX
   * re-proposes it. */
  dismissed: boolean;
}

export type Confidence = "high" | "medium" | "low";

export interface MoveProposal {
  id: number;
  /** Class-relative; "_Inbox/…" for sort-job rows, anywhere for chat rows. */
  sourceRelPath: string;
  /** Class-relative target including the file name. */
  destRelPath: string;
  reasoning: string;
  /** null on chat proposals — sort jobs always fill it in. */
  confidence: Confidence | null;
  source: "chat" | "sort_job";
  createdAt: number;
}

export interface SortState {
  inbox: InboxFile[];
  proposals: MoveProposal[];
}

export interface StageResult {
  staged: string[];
  skippedFolders: number;
  jobId: number | null;
}

export function getSortState(classId: number): Promise<SortState> {
  return invoke<SortState>("sort_state", { classId });
}

/** Manual sort trigger: retry after a failure, or files left in the inbox. */
export function runSortJob(classId: number): Promise<number> {
  return invoke<number>("run_sort_job", { classId });
}

/** Approve the move (optionally to a picked folder) or leave the file put. */
export function resolveProposal(
  proposalId: number,
  approve: boolean,
  destOverride?: string,
): Promise<string> {
  return invoke<string>("resolve_move_proposal", {
    proposalId,
    approve,
    destOverride: destOverride ?? null,
  });
}

// --- Native drag-drop (SPEC §10 step 1) --------------------------------------
// One webview-level listener for the whole app. The open class workspace
// declares itself the drop target during render (a module variable, not React
// state — only these native handlers read it); the store below drives the
// drop-zone highlight. Module-level wiring, no useEffect (workspace rules).

export interface DragSnapshot {
  /** A drag is hovering the window while a class workspace is open. */
  active: boolean;
  /** Last staging failure, surfaced in the inbox section. */
  error: string | null;
}

let snapshot: DragSnapshot = { active: false, error: null };
const listeners = new Set<() => void>();

function emitDrag(patch: Partial<DragSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  for (const notify of listeners) notify();
}

let dropClassId: number | null = null;

/** Called by App during render: the open workspace's class, or null. */
export function setDropTarget(classId: number | null) {
  dropClassId = classId;
  if (classId === null && snapshot.active) emitDrag({ active: false });
}

export function clearDropError() {
  if (snapshot.error !== null) emitDrag({ error: null });
}

void getCurrentWebview().onDragDropEvent((event) => {
  if (dropClassId === null) return;
  const type = event.payload.type;
  if (type === "enter" || type === "over") {
    if (!snapshot.active) emitDrag({ active: true });
  } else if (type === "leave") {
    emitDrag({ active: false });
  } else if (type === "drop") {
    emitDrag({ active: false, error: null });
    // Success needs no handling here: the backend emits hub-changed, which
    // refetches the queue and the card badges (lib/query.ts).
    void invoke<StageResult>("stage_inbox_files", {
      classId: dropClassId,
      paths: event.payload.paths,
    }).catch((e) => emitDrag({ error: String(e) }));
  }
});

export function useDragState(): DragSnapshot {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => snapshot,
  );
}
