import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, Megaphone } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import {
  listAnnouncements,
  setAnnouncementActionDone,
  type Announcement,
  type AnnouncementAction,
} from "@/lib/canvas";
import { formatDueDate } from "@/lib/schedule";
import {
  checkCircle,
  checkCircleDone,
  chipMuted,
  errorLine,
  meta,
  readingText,
} from "@/lib/styles";

/** The one query the section and the workspace's nav share — TanStack dedupes
 *  it, so the nav's link costs no second request. */
export function announcementsQuery(classId: number) {
  return {
    queryKey: ["announcements", classId] as const,
    queryFn: () => listAnnouncements(classId),
    placeholderData: (prev: Announcement[] | undefined) => prev,
  };
}

/**
 * SPEC §7.2 — what the professor said. The class's Canvas announcements,
 * newest first, as a record rather than a queue: nothing here is unread,
 * nothing waits on a decision, and the section is absent until a sync has
 * brought one across. Bodies are Canvas's HTML stripped to text, and the
 * app never renders Canvas's HTML.
 */
export function NoticesSection({ classId }: { classId: number }) {
  const { data: notices } = useQuery(announcementsQuery(classId));
  if (notices === undefined || notices.length === 0) return null;

  return (
    <section id="notices" className="mt-14 scroll-mt-20" aria-label="Notices">
      <SectionHeading
        title="Notices"
        count={
          notices.length === 1 ? "1 from Canvas" : `${notices.length} from Canvas`
        }
      />
      <div className="mt-3">
        {notices.map((notice) => (
          <NoticeRow key={notice.id} notice={notice} />
        ))}
      </div>
    </section>
  );
}

/** One announcement: its title and when it was posted, with the text under
 *  it clamped to a few lines until the row is opened. */
function NoticeRow({ notice }: { notice: Announcement }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border-b border-border/70 py-3 last:border-b-0">
      <button
        type="button"
        aria-expanded={open}
        title={open ? "Collapse" : "Read the whole notice"}
        onClick={() => setOpen((prev) => !prev)}
        className="flex w-full cursor-pointer items-baseline gap-2.5 rounded-sm text-left focus-visible:outline-2 focus-visible:outline-(--accent)"
      >
        <Megaphone
          size={14}
          aria-hidden
          className="shrink-0 translate-y-0.5 text-muted-foreground"
        />
        <span className="min-w-0 flex-1 truncate text-[15px] font-medium">
          {notice.title}
        </span>
        {/* `Sep 2 at 3:03 pm`, with the year when the notice is from another one —
            a course site carries a term's worth of old posts, and "Aug 20" alone
            reads as this term's. */}
        <span className={`shrink-0 ${meta}`}>{formatDueDate(notice.postedAt)}</span>
      </button>
      {notice.body !== "" && (
        <p
          className={`mt-1.5 pl-6.5 whitespace-pre-line ${readingText} text-muted-foreground ${
            open ? "" : "line-clamp-3"
          }`}
        >
          {notice.body}
        </p>
      )}
      {notice.actions.length > 0 && (
        <ul className="mt-2 space-y-1 pl-6.5" aria-label={`What ${notice.title} asks`}>
          {notice.actions.map((action) => (
            <ActionLine key={action.id} action={action} />
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * One line the announcement scan read out of the notice (SPEC §7.2): a
 * to-do with the deadline row's ring, which is its own way back, or a
 * change, which is said and needs nothing. Always in view, clamped body or
 * not, since the point of the line is to be acted on.
 */
function ActionLine({ action }: { action: AnnouncementAction }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (action.kind === "change") {
    return (
      <li className="flex items-baseline gap-2 text-body">
        <span className={chipMuted}>change</span>
        <span className="min-w-0 text-muted-foreground">{action.text}</span>
      </li>
    );
  }
  const toggle = () => {
    setBusy(true);
    setError(null);
    setAnnouncementActionDone(action.id, !action.done)
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };
  return (
    <li className="flex items-baseline gap-2 text-body">
      <button
        type="button"
        role="checkbox"
        aria-checked={action.done}
        aria-label={action.done ? `Reopen ${action.text}` : `Mark ${action.text} done`}
        title={action.done ? "Reopen this to-do" : "Mark done"}
        disabled={busy}
        onClick={toggle}
        className={
          `${checkCircle} translate-y-0.5 cursor-pointer focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none ` +
          (action.done ? checkCircleDone : "border-muted-foreground/40 hover:border-(--accent)")
        }
      >
        {action.done && <Check size={10} strokeWidth={3} aria-hidden />}
      </button>
      <span
        className={`min-w-0 ${action.done ? "text-muted-foreground line-through" : ""}`}
      >
        {action.text}
      </span>
      {error && <span className={`${errorLine} mt-0`}>{error}</span>}
    </li>
  );
}
