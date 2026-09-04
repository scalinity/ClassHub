import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Megaphone } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import { listAnnouncements, type Announcement } from "@/lib/canvas";
import { formatDueDate } from "@/lib/schedule";
import { meta, readingText } from "@/lib/styles";

/** The one query the section and the workspace's nav share — TanStack dedupes
 *  it, so the nav's link costs no second request. */
export function announcementsQuery(classId: number) {
  return {
    queryKey: ["announcements", classId] as const,
    queryFn: () => listAnnouncements(classId),
    placeholderData: (prev: Announcement[] | undefined) => prev,
  };
}

/** `Sep 2 at 3:03 pm`, and `Sep 2, 2025 at 3:03 pm` when the notice is from
 *  another year — a course site can carry a term's worth of old posts, and
 *  "Aug 20" alone reads as this term's. */
function noticeStamp(postedAt: string): string {
  return formatDueDate(postedAt);
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
        <span className={`shrink-0 ${meta}`}>{noticeStamp(notice.postedAt)}</span>
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
    </div>
  );
}
