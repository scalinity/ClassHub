import { useState, type CSSProperties, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import { CLASS_ACCENTS } from "@/lib/classes";
import { listDeadlines, setDeadlineStatus, type Deadline } from "@/lib/deadlines";
import { queryClient } from "@/lib/query";
import { daysUntil, dueDayLabel, isOverdue } from "@/lib/schedule";
import { checkCircle, checkCircleDone, errorLine, readingText } from "@/lib/styles";

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
  if (data === undefined) return null;
  const due = data.filter((d) => d.status === "open" && daysUntil(d.dueAt) < 7).length;

  return (
    <section className="mt-12" aria-label="Deadlines in the next 7 days">
      <SectionHeading
        title="Due in the next 7 days"
        count={due === 0 ? undefined : due === 1 ? "1 due" : `${due} due`}
      />
      <DueChips
        within={7}
        empty={
          <p className={`mt-3 ${readingText} text-muted-foreground`}>
            Nothing due in the next 7 days.
          </p>
        }
        wrap={(chips) => <div className="mt-4">{chips}</div>}
      />
    </section>
  );
}

/**
 * The chips for every open deadline due within `within` days, overdue ones
 * included — the strip's, and Today's for the next three days (SPEC §12).
 * A chip marked done here stays as done until the dashboard is next opened,
 * a second click reopening it; `wrap` frames the chips when there are any,
 * and `empty` stands in when there are none.
 */
export function DueChips({
  within,
  empty,
  wrap,
}: {
  within: number;
  empty: ReactNode;
  wrap: (chips: ReactNode) => ReactNode;
}) {
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
  // One failure per chip, kept until that chip is clicked again: a second
  // chip's click must not clear a message the reader has not seen.
  const [errors, setErrors] = useState<ReadonlyMap<number, string>>(
    () => new Map(),
  );
  const keep = (id: number, kept: boolean) =>
    setCompletedHere((prev) => {
      const next = new Set(prev);
      if (kept) next.add(id);
      else next.delete(id);
      return next;
    });
  const report = (id: number, message: string | null) =>
    setErrors((prev) => {
      const next = new Map(prev);
      if (message === null) next.delete(id);
      else next.set(id, message);
      return next;
    });
  if (data === undefined) return null;

  const shown = data.filter(
    (d) =>
      (d.status === "open" || completedHere.has(d.id)) && daysUntil(d.dueAt) < within,
  );
  if (shown.length === 0) return <>{empty}</>;

  return wrap(
    <>
      {[...errors].map(([id, message]) => (
        <p key={id} className={`${errorLine} mt-0 mb-3`}>
          {data.find((d) => d.id === id)?.title ?? "A deadline"}: {message}
        </p>
      ))}
      <div className="flex flex-wrap gap-2">
        {shown.map((deadline) => (
          <DeadlineChip
            key={deadline.id}
            deadline={deadline}
            onKeep={keep}
            onError={report}
          />
        ))}
      </div>
    </>,
  );
}

function DeadlineChip({
  deadline,
  onKeep,
  onError,
}: {
  deadline: Deadline;
  /** Whether the strip keeps this chip once its deadline is done. */
  onKeep: (id: number, kept: boolean) => void;
  onError: (id: number, message: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);
  const done = deadline.status === "done";
  const overdue = !done && isOverdue(deadline.dueAt);
  const style = {
    "--accent": CLASS_ACCENTS[deadline.classColor] ?? "var(--class-blue)",
  } as CSSProperties;

  const toggle = () => {
    setBusy(true);
    onError(deadline.id, null);
    // Kept before the write lands, so the refetch that follows it cannot
    // drop the chip between the click and its answer.
    if (!done) onKeep(deadline.id, true);
    setDeadlineStatus(deadline.id, !done)
      // Held until the strip has re-read the deadlines: the push that follows
      // the write invalidates the same query, but a chip reading open a beat
      // after its click could take a second one.
      .then(() => queryClient.invalidateQueries({ queryKey: ["deadlines"] }))
      .then(() => setBusy(false))
      .catch((e) => {
        // The write did not land, so the strip must not keep the chip: a
        // completion from the tab later would bring it back as done.
        if (!done) onKeep(deadline.id, false);
        onError(deadline.id, String(e));
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
          `${checkCircle} ` +
          (done ? checkCircleDone : "border-(--accent-ink)/40 group-hover:border-(--accent)")
        }
      >
        {done && <Check size={10} strokeWidth={3} />}
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
