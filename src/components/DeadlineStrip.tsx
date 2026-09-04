import type { CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";

import { SectionHeading } from "@/components/SectionHeading";
import { CLASS_ACCENTS } from "@/lib/classes";
import { listDeadlines, type Deadline } from "@/lib/deadlines";
import { daysUntil, dueDayLabel } from "@/lib/schedule";
import { readingText } from "@/lib/styles";

/**
 * SPEC §11 — the dashboard's next-7-days deadline strip: every open deadline
 * across the hub due inside a week, overdue ones included and first (they are
 * the most urgent thing on the dashboard). Each chip is washed in its class's
 * colour, which ties it to the card below.
 */
export function DeadlineStrip() {
  const { data } = useQuery({
    queryKey: ["deadlines"],
    queryFn: listDeadlines,
  });
  if (data === undefined) return null;

  const upcoming = data.filter(
    (d) => d.status === "open" && daysUntil(d.dueAt) < 7,
  );

  return (
    <section className="mt-12" aria-label="Deadlines in the next 7 days">
      <SectionHeading
        title="Due in the next 7 days"
        count={
          upcoming.length === 0
            ? undefined
            : upcoming.length === 1
              ? "1 due"
              : `${upcoming.length} due`
        }
      />
      {upcoming.length === 0 ? (
        <p className={`mt-3 ${readingText} text-muted-foreground`}>
          Nothing due in the next 7 days.
        </p>
      ) : (
        <div className="mt-4 flex flex-wrap gap-2">
          {upcoming.map((deadline) => (
            <DeadlineChip key={deadline.id} deadline={deadline} />
          ))}
        </div>
      )}
    </section>
  );
}

function DeadlineChip({ deadline }: { deadline: Deadline }) {
  const overdue = daysUntil(deadline.dueAt) < 0;
  const style = {
    "--accent": CLASS_ACCENTS[deadline.classColor] ?? "var(--class-blue)",
  } as CSSProperties;

  return (
    <span
      style={style}
      title={`${deadline.title} — ${deadline.className}`}
      className="flex max-w-full items-baseline gap-1.5 rounded-md bg-(--accent)/10 px-2.5 py-1.5 text-body"
    >
      <span className="min-w-0 truncate font-medium text-(--accent-ink)">
        {deadline.title}
      </span>
      <span
        className={
          "shrink-0 tabular-nums " +
          (overdue ? "text-destructive" : "text-muted-foreground")
        }
      >
        · {dueDayLabel(deadline.dueAt)}
      </span>
    </span>
  );
}
