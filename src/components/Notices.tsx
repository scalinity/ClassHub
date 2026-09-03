import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Megaphone } from "lucide-react";

import { listAnnouncements, type Announcement } from "@/lib/canvas";
import { formatDueDate } from "@/lib/schedule";

/**
 * SPEC §7.2 — what the professor said. The class's Canvas announcements,
 * newest first, as a record rather than a queue: nothing here is unread,
 * nothing waits on a decision, and the section is absent until a sync has
 * brought one across. Bodies are Canvas's HTML stripped to text, and the
 * app never renders Canvas's HTML.
 */
export function NoticesSection({ classId }: { classId: number }) {
  const { data: notices } = useQuery({
    queryKey: ["announcements", classId],
    queryFn: () => listAnnouncements(classId),
    placeholderData: (prev) => prev,
  });
  if (notices === undefined || notices.length === 0) return null;

  return (
    <section className="mt-12" aria-label="Notices">
      <div className="flex items-baseline justify-between border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          NOTICES
        </h2>
        <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
          {notices.length === 1
            ? "1 FROM CANVAS"
            : `${notices.length} FROM CANVAS`}
        </span>
      </div>
      <div className="mt-3 space-y-1">
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
    <div className="rounded-md px-2 py-1.5 transition-colors hover:bg-muted/60">
      <button
        type="button"
        aria-expanded={open}
        title={open ? "Collapse" : "Read the whole notice"}
        onClick={() => setOpen((prev) => !prev)}
        className="flex w-full cursor-pointer items-center gap-2 text-left focus-visible:outline-2 focus-visible:outline-(--accent)"
      >
        <Megaphone
          size={14}
          aria-hidden
          className="shrink-0 text-muted-foreground/80"
        />
        <span className="min-w-0 flex-1 truncate text-[13px] font-medium">
          {notice.title}
        </span>
        <span className="shrink-0 font-mono text-[10px] tracking-[0.1em] text-muted-foreground">
          {formatDueDate(notice.postedAt)}
        </span>
      </button>
      {notice.body !== "" && (
        <p
          className={`mt-1 pl-6 text-[12.5px] leading-relaxed whitespace-pre-line text-muted-foreground ${
            open ? "" : "line-clamp-3"
          }`}
        >
          {notice.body}
        </p>
      )}
    </div>
  );
}
