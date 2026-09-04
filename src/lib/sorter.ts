import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useSyncExternalStore } from "react";

/** SPEC §4: the drop folder, mirrored from sorter.rs INBOX_DIR — the one path
 *  string both sides must agree on. */
export const INBOX_DIR = "_Inbox";

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
  /** null on chat proposals — sort jobs and Canvas always fill it in. */
  confidence: Confidence | null;
  /** Where the proposal came from: chat, a sort job reading the file's
   *  contents, Canvas telling the app which folder it filed the file in, or
   *  the file's own name carrying a week, clicked on its Materials row. */
  source: "chat" | "sort_job" | "canvas" | "by_name";
  createdAt: number;
  /** On a Canvas card whose destination names a week — in the file's name, a
   *  module the course reads as a week, or Canvas's own folder — the week
   *  folder as a second destination (SPEC §10), derived on every read and
   *  never stored. Absent on every other card. */
  alternative?: WeekAlternative;
}

/** The week folder a Canvas card also offers: where the file's row would
 *  propose it once it landed where Canvas put it, offered a step earlier. */
export interface WeekAlternative {
  week: number;
  destRelPath: string;
  /** The by-name card's own words: the reading, and the division that
   *  counts the file under that folder. */
  reasoning: string;
}

export interface SortState {
  inbox: InboxFile[];
  proposals: MoveProposal[];
}

export interface StageResult {
  staged: string[];
  skippedFolders: number;
  /** Per-file staging failures ("name: reason") — the batch survives them. */
  failed: string[];
  jobId: number | null;
}

export function getSortState(classId: number): Promise<SortState> {
  return invoke<SortState>("get_sort_state", { classId });
}

/** Manual sort trigger: retry after a failure, or files left in the inbox. */
export function runSortJob(classId: number): Promise<number> {
  return invoke<number>("run_sort_job", { classId });
}

/** SORT BY CONTENT on a Canvas card: a sort job over that one file, whose
 *  destination replaces the folder Canvas filed it in. The job's scope is the
 *  file's inbox path, which is how the card knows the sort is its own. */
export function sortByContent(proposalId: number): Promise<number> {
  return invoke<number>("sort_by_content", { proposalId });
}

/** What a filing click wrote (SPEC §10): where it lands — a file's own
 *  destination, or a folder's under the week folder — and how many cards. */
export interface WeekFiling {
  destRel: string;
  cards: number;
}

/** A file or folder named for a week, proposed into that week's folder from
 *  its row in Materials (SPEC §10). The cards land in the inbox queue and
 *  approval is the ordinary move. */
export function proposeWeekFiling(
  classId: number,
  relPath: string,
): Promise<WeekFiling> {
  return invoke<WeekFiling>("propose_week_filing", { classId, relPath });
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

export interface DropNotice {
  /** The class whose workspace the drop targeted — only that workspace renders it. */
  classId: number;
  message: string;
}

export interface DragSnapshot {
  /** A drag is hovering the window while a class workspace is open. */
  active: boolean;
  /** Last staging failure or partial-drop notice, scoped to its class. */
  notice: DropNotice | null;
}

let snapshot: DragSnapshot = { active: false, notice: null };
const listeners = new Set<() => void>();

function emitDrag(patch: Partial<DragSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  for (const notify of listeners) notify();
}

let dropClassId: number | null = null;
let dropInterceptor: ((paths: string[]) => void) | null = null;

/**
 * Called by App during render: the open workspace's class, or null. A pure
 * assignment — notifying store subscribers here would be a state update
 * during App's render pass, so a drag that outlives its workspace is reset
 * by the next drag event (below) instead.
 */
export function setDropTarget(classId: number | null) {
  dropClassId = classId;
  // Navigating away can strand a dialog that had claimed drops (Add lecture,
  // left open when the workspace closes), and a stale claim would silently
  // swallow every later drop. Leaving a view always releases it.
  dropInterceptor = null;
}

/**
 * The class whose workspace is open, or null on the dashboard and in Settings.
 * The same value the drop target reads: chat sends it with each question so a
 * question that names no class means this one (SPEC §9), and the ask panel
 * draws its starters from it.
 */
export function openClassId(): number | null {
  return dropClassId;
}

export function clearDropNotice() {
  if (snapshot.notice !== null) emitDrag({ notice: null });
}

/**
 * Lets an open dialog claim dropped files for itself instead of staging them
 * to the inbox — Add lecture uses it so dropping a recording onto the form
 * fills the form, rather than silently starting a sort. Same render-time
 * assignment contract as `setDropTarget`, and cleared when the dialog closes.
 */
export function setDropInterceptor(fn: ((paths: string[]) => void) | null) {
  dropInterceptor = fn;
}

void getCurrentWebview().onDragDropEvent((event) => {
  if (dropClassId === null) {
    if (snapshot.active) emitDrag({ active: false });
    return;
  }
  const classId = dropClassId;
  const type = event.payload.type;
  if (type === "enter" || type === "over") {
    // A dialog that has claimed drops owns the affordance too: showing the
    // sort overlay on top of it would promise the wrong outcome.
    if (!snapshot.active && dropInterceptor === null) emitDrag({ active: true });
  } else if (type === "leave") {
    emitDrag({ active: false });
  } else if (type === "drop") {
    emitDrag({ active: false, notice: null });
    if (dropInterceptor !== null) {
      dropInterceptor(event.payload.paths);
      return;
    }
    // Staged files announce themselves through the backend's hub-changed
    // push; what needs surfacing here is everything that did NOT stage.
    void invoke<StageResult>("stage_inbox_files", {
      classId,
      paths: event.payload.paths,
    })
      .then((result) => {
        const parts: string[] = [];
        if (result.skippedFolders > 0) {
          parts.push(
            result.skippedFolders === 1
              ? "1 folder skipped — drop files, not folders"
              : `${result.skippedFolders} folders skipped — drop files, not folders`,
          );
        }
        parts.push(...result.failed);
        if (parts.length > 0) {
          emitDrag({ notice: { classId, message: parts.join(" · ") } });
        }
      })
      .catch((e) => emitDrag({ notice: { classId, message: String(e) } }));
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
