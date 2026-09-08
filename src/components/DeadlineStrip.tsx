import { useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import { CLASS_ACCENTS } from "@/lib/classes";
import { listDeadlines, setDeadlineStatus, type Deadline } from "@/lib/deadlines";
import { queryClient } from "@/lib/query";
import { daysUntil, dueDayLabel } from "@/lib/schedule";
import { errorLine, readingText } from "@/lib/styles";

/**
 * SPEC §11 — the dashboard's next-7-days deadline strip: every open deadline
 * across the hub due inside a week, overdue ones included and first (they are
 * the most urgent thing on the dashboard). Each chip is washed in its class's
 * colour, which ties it to the card below, and is the Deadlines tab's own
 * control at chip scale: a click marks the deadline done through the same
 * command, so the two surfaces share one audit row and one query.
 */
export function DeadlineStrip() {
  const { data } = useQuery({
    queryKey: ["deadlines"],
    queryFn: listDeadlines,
  });
  // The deadlines this strip marked done while the dashboard has been open. A
  // done deadline leaves the strip, so these stay as done chips a second click
  // reopens — the way back, in place of a confirm — until the dashboard is
  // next opened. The data still decides what a chip reads: one the tab
  // reopened reads open, and one deleted is gone.
  const [completedHere, setCompletedHere] = useState<ReadonlySet<number>>(
    () => new Set(),
  );
  const [error, setError] = useState<string | null>(null);
  if (data === undefined) return null;

  const shown = data.filter(
    (d) =>
      (d.status === "open" || completedHere.has(d.id)) && daysUntil(d.dueAt) < 7,
  );
  const due = shown.filter((d) => d.status === "open").length;

  return (
    <section className="mt-12" aria-label="Deadlines in the next 7 days">
      <SectionHeading
        title="Due in the next 7 days"
        count={due === 0 ? undefined : due === 1 ? "1 due" : `${due} due`}
      >
        {error && <p className={errorLine}>{error}</p>}
      </SectionHeading>
      {shown.length === 0 ? (
        <p className={`mt-3 ${readingText} text-muted-foreground`}>
          Nothing due in the next 7 days.
        </p>
      ) : (
        <div className="mt-4 flex flex-wrap gap-2">
          {shown.map((deadline) => (
            <DeadlineChip
              key={deadline.id}
              deadline={deadline}
              onCompleted={(id) =>
                setCompletedHere((prev) => new Set(prev).add(id))
              }
              onError={setError}
            />
          ))}
        </div>
      )}
    </section>
  );
}

function DeadlineChip({
  deadline,
  onCompleted,
  onError,
}: {
  deadline: Deadline;
  onCompleted: (id: number) => void;
  onError: (message: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);
  const done = deadline.status === "done";
  const overdue = !done && daysUntil(deadline.dueAt) < 0;
  const style = {
    "--accent": CLASS_ACCENTS[deadline.classColor] ?? "var(--class-blue)",
  } as CSSProperties;

  const toggle = () => {
    setBusy(true);
    onError(null);
    // Kept before the write lands, so the refetch that follows it cannot
    // drop the chip between the click and its answer.
    if (!done) onCompleted(deadline.id);
    setDeadlineStatus(deadline.id, !done)
      // Held until the strip has re-read the deadlines: the push that follows
      // the write invalidates the same query, but a chip reading open a beat
      // after its click could take a second one.
      .then(() => queryClient.invalidateQueries({ queryKey: ["deadlines"] }))
      .then(() => setBusy(false))
      .catch((e) => {
        onError(String(e));
        setBusy(false);
      });
  };

  return (
    <button
      type="button"
      style={style}
      disabled={busy}
      onClick={toggle}
      title={
        `${deadline.title} — ${deadline.className}` +
        (deadline.canvasAssignmentId !== null ? " · from Canvas" : "")
      }
      // The Deadlines tab's own words for its checkbox (Deadlines.tsx,
      // DeadlineRow): one control in two places reads the same.
      aria-label={
        done ? `Reopen ${deadline.title}` : `Mark ${deadline.title} done`
      }
      className={
        "group flex max-w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-body transition-colors focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-60 " +
        (done ? "bg-muted/60" : "bg-(--accent)/10 hover:bg-(--accent)/15")
      }
    >
      <span
        aria-hidden
        className={
          "flex size-3.5 shrink-0 items-center justify-center rounded-full border transition-colors " +
          (done
            ? "border-(--accent) bg-(--accent) text-white dark:text-background"
            : "border-(--accent-ink)/40 group-hover:border-(--accent)")
        }
      >
        {done && <Check size={9} strokeWidth={3} />}
      </span>
      <span
        className={
          "min-w-0 truncate font-medium " +
          (done ? "text-muted-foreground line-through" : "text-(--accent-ink)")
        }
      >
        {deadline.title}
      </span>
      <span
        className={
          "shrink-0 tabular-nums " +
          (overdue ? "text-destructive" : "text-muted-foreground")
        }
      >
        · {done ? "done" : dueDayLabel(deadline.dueAt)}
      </span>
    </button>
  );
}
