import { X } from "lucide-react";

import { dismissToast, undoNotice, useNotices, type Notice } from "@/lib/notices";
import { buttonIconNeutral, buttonTextNeutral } from "@/lib/styles";

/**
 * SPEC §12 — the notice at the bottom of the window after a reversible
 * action: what happened, and `Undo`. On the window, not the section, so a
 * batch's notice survives a workspace change; it fades after a while, and
 * the Job Center's panel keeps the last few.
 */
export function NoticeToast() {
  const { toast } = useNotices();
  if (toast === null) return null;
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-4 z-40 flex justify-start px-4">
      <div
        role="status"
        className="pointer-events-auto flex max-w-md items-center gap-2 rounded-xl bg-surface py-1.5 pr-1.5 pl-3.5 shadow-lg ring-1 ring-border animate-in fade-in slide-in-from-bottom-2 duration-200 motion-reduce:animate-none"
      >
        <NoticeLine notice={toast} />
        <button
          type="button"
          aria-label="Dismiss this notice"
          onClick={dismissToast}
          className={buttonIconNeutral}
        >
          <X size={12} aria-hidden />
        </button>
      </div>
    </div>
  );
}

/** The words and the `Undo`, shared by the toast and the Job Center's list. */
export function NoticeLine({ notice }: { notice: Notice }) {
  const undoable = notice.auditIds.length > 0 && notice.state === "shown";
  return (
    <>
      <span className="min-w-0 flex-1 text-body">
        {notice.state === "undone" ? (
          <>
            <span className="text-muted-foreground">Undone · </span>
            {notice.text}
          </>
        ) : (
          notice.text
        )}
        {notice.detail && (
          <span className="block text-fine text-destructive">
            {notice.state === "refused" ? "Not undone: " : "Partly: "}
            {notice.detail}
          </span>
        )}
      </span>
      {undoable && (
        <button
          type="button"
          onClick={() => void undoNotice(notice)}
          className={`${buttonTextNeutral} font-semibold text-foreground`}
        >
          Undo
        </button>
      )}
      {notice.state === "undoing" && (
        <span className="px-2 text-body text-muted-foreground">Undoing…</span>
      )}
    </>
  );
}
