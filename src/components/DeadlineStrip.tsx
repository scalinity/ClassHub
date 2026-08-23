import type { CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";

import { CLASS_ACCENTS } from "@/lib/classes";
import { listDeadlines, type Deadline } from "@/lib/deadlines";
import { daysUntil, dueDayLabel } from "@/lib/schedule";

/**
 * SPEC §11 — the dashboard's next-7-days deadline strip: every open deadline
 * across the hub due inside a week, overdue ones included and first (they are
 * the most urgent thing on the dashboard). The accent dot ties each chip to
 * its class card below.
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
    <section className="mt-10" aria-label="Deadlines in the next 7 days">
      <div className="flex items-baseline justify-between border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          NEXT 7 DAYS
        </h2>
        {upcoming.length > 0 && (
          <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
            {upcoming.length === 1 ? "1 DUE" : `${upcoming.length} DUE`}
          </span>
        )}
      </div>
      {upcoming.length === 0 ? (
        <p className="mt-3 text-[13px] text-muted-foreground">
          Nothing due in the next 7 days.
        </p>
      ) : (
        <div className="mt-3 flex flex-wrap gap-2">
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
      className="flex max-w-full items-center gap-2 rounded-lg border bg-card px-3 py-2"
    >
      <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-(--accent)" />
      <span
        className={
          "shrink-0 font-mono text-[10px] font-medium tracking-[0.1em] " +
          (overdue ? "text-destructive" : "text-(--accent)")
        }
      >
        {dueDayLabel(deadline.dueAt)}
      </span>
      <span className="min-w-0 truncate text-[12.5px]">{deadline.title}</span>
    </span>
  );
}
