import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

/**
 * SPEC §12 — the notice that follows a reversible action, with `Undo`.
 *
 * The backend emits one after every write it can reverse — a command, a chat
 * tool, the sync — carrying the audit rows it wrote (SPEC §6), so every
 * surface reaches the same notice. The toast at the bottom of the window
 * shows the latest for a while; the last few stay reachable from the Job
 * Center's panel. Module-level store, no useEffect (workspace rules).
 */

export interface Notice {
  /** Local, for keys and lookups. */
  key: number;
  text: string;
  /** The audit rows `Undo` reverses, in the order they were written. */
  auditIds: number[];
  classId: number | null;
  /** Unix milliseconds. */
  at: number;
  state: "shown" | "undoing" | "undone" | "refused";
  /** What the undo refused, when it did. */
  detail: string | null;
}

interface NoticeEvent {
  text: string;
  auditIds: number[];
  classId?: number;
}

export interface UndoOutcome {
  undone: number[];
  refused: string[];
}

export interface NoticesSnapshot {
  /** Newest first, capped. */
  notices: readonly Notice[];
  /** The one the toast shows, until it fades or is dismissed. */
  toast: Notice | null;
}

/** How long a toast stays; an undone or refused toast stays a little longer. */
const TOAST_MS = 9000;
const KEEP = 8;

let snapshot: NoticesSnapshot = { notices: [], toast: null };
const listeners = new Set<() => void>();
let nextKey = 1;
let fade: ReturnType<typeof setTimeout> | null = null;

function emit(patch: Partial<NoticesSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  for (const notify of listeners) notify();
}

function update(key: number, patch: Partial<Notice>) {
  const notices = snapshot.notices.map((n) =>
    n.key === key ? { ...n, ...patch } : n,
  );
  const toast =
    snapshot.toast?.key === key ? { ...snapshot.toast, ...patch } : snapshot.toast;
  emit({ notices, toast });
}

/** Restarts the toast's fade, for the notice that owns it. One timer serves
 *  the one toast; an undo of an older notice from the panel must not touch
 *  it, or the toast on screen would lose its fade and stay. */
function scheduleFade(key: number, ms: number) {
  if (snapshot.toast?.key !== key) return;
  if (fade !== null) clearTimeout(fade);
  fade = setTimeout(() => {
    if (snapshot.toast?.key === key) emit({ toast: null });
  }, ms);
}

function push(event: NoticeEvent) {
  const notice: Notice = {
    key: nextKey++,
    text: event.text,
    auditIds: event.auditIds,
    classId: event.classId ?? null,
    at: Date.now(),
    state: "shown",
    detail: null,
  };
  emit({
    notices: [notice, ...snapshot.notices].slice(0, KEEP),
    toast: notice,
  });
  scheduleFade(notice.key, TOAST_MS);
}

void listen<NoticeEvent>("notice", ({ payload }) => push(payload));

/** The toast's own close. */
export function dismissToast() {
  if (snapshot.toast !== null) emit({ toast: null });
}

/**
 * Reverses the notice's audit rows (SPEC §6). The refetches follow the hub
 * pushes the backend emits; the notice records what happened so the toast
 * and the panel read `Undone` or the refusal.
 */
export function undoNotice(notice: Notice): Promise<void> {
  if (notice.auditIds.length === 0 || notice.state !== "shown") {
    return Promise.resolve();
  }
  update(notice.key, { state: "undoing" });
  return invoke<UndoOutcome>("undo_audit", { auditIds: notice.auditIds })
    .then((outcome) => {
      if (outcome.refused.length === 0) {
        update(notice.key, { state: "undone", detail: null });
      } else {
        update(notice.key, {
          state: outcome.undone.length > 0 ? "undone" : "refused",
          detail: outcome.refused.join(" · "),
        });
      }
      scheduleFade(notice.key, TOAST_MS);
    })
    .catch((e) => {
      update(notice.key, { state: "refused", detail: String(e) });
      scheduleFade(notice.key, TOAST_MS);
    });
}

export function useNotices(): NoticesSnapshot {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => snapshot,
  );
}
