import type { CSSProperties } from "react";

import { CLASS_ACCENTS, type ClassInfo, type Meeting } from "@/lib/classes";
import {
  daysUntil,
  formatDueDate,
  formatTime,
  formatTimeRange,
  isoWeekday,
  nextMeeting,
  toMinutes,
  weekdayLabel,
  type NextMeeting,
} from "@/lib/schedule";

const DAY_LABELS = ["MON", "TUE", "WED", "THU", "FRI"];
/** Vertical scale of the time canvas. */
const PX_PER_HOUR = 26;

interface Block {
  cls: ClassInfo;
  meeting: Meeting;
  inSession: boolean;
}

/**
 * SPEC §11 — the weekly schedule grid: the class cards' M T W T F tick row
 * grown into a time-true instrument. Five weekday columns over a proportional
 * time canvas with mono hour rules; meetings render as accent slabs placed by
 * their real start and duration, today's column is washed, and a meeting in
 * session inverts to solid accent (the same inversion as the card chip).
 */
export function WeekSchedule({ classes }: { classes: ClassInfo[] }) {
  const now = new Date();
  const today = isoWeekday(now);
  const nowMinutes = now.getHours() * 60 + now.getMinutes();

  // The section stands whenever anything meets at all — the next-class chip
  // considers all seven days, so a weekend-only schedule must not hide it.
  // Only the Mon–Fri canvas needs weekday blocks.
  if (classes.every((cls) => cls.meetings.length === 0)) return null;

  const blocks: Block[] = classes.flatMap((cls) =>
    cls.meetings
      .filter((m) => m.weekday >= 1 && m.weekday <= 5)
      .map((meeting) => ({
        cls,
        meeting,
        inSession:
          meeting.weekday === today &&
          nowMinutes >= toMinutes(meeting.startTime) &&
          nowMinutes < toMinutes(meeting.endTime),
      })),
  );

  const exams = classes.filter(
    (cls) => cls.finalExamStart && daysUntil(cls.finalExamStart) >= 0,
  );

  return (
    <section className="mt-10" aria-label="Weekly schedule">
      <div className="flex items-baseline justify-between gap-4 border-b pb-3">
        <h2 className="shrink-0 font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          THIS WEEK
        </h2>
        <NextClassChip classes={classes} />
      </div>

      {blocks.length > 0 ? (
        <WeekCanvas blocks={blocks} today={today} />
      ) : (
        <p className="mt-3 text-[13px] text-muted-foreground">
          Nothing meets on weekdays.
        </p>
      )}

      {exams.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-2 pl-10">
          {exams.map((cls) => (
            <ExamChip key={cls.id} cls={cls} />
          ))}
        </div>
      )}
    </section>
  );
}

/** The Mon–Fri time canvas: axis math needs at least one block. */
function WeekCanvas({ blocks, today }: { blocks: Block[]; today: number }) {
  // Axis: the meetings' span, widened to whole hours.
  const axisStart =
    Math.floor(
      Math.min(...blocks.map((b) => toMinutes(b.meeting.startTime))) / 60,
    ) * 60;
  const axisEnd =
    Math.ceil(Math.max(...blocks.map((b) => toMinutes(b.meeting.endTime))) / 60) *
    60;
  // Never zero: meetings whose start equals their end (possible only via
  // hand-edited rows) would otherwise make pct() emit NaN geometry.
  const span = Math.max(axisEnd - axisStart, 60);
  const canvasHeight = (span / 60) * PX_PER_HOUR;
  const pct = (minutes: number) => ((minutes - axisStart) / span) * 100;

  // Rules at even clock hours, skipping the canvas edges.
  const hourMarks: number[] = [];
  for (let h = Math.ceil(axisStart / 60); h * 60 <= axisEnd; h++) {
    const pos = pct(h * 60);
    if (h % 2 === 0 && pos > 3 && pos < 97) hourMarks.push(h);
  }

  return (
    <div className="mt-4">
      <div className="grid grid-cols-5 pb-2 pl-10">
        {DAY_LABELS.map((label, i) => (
          <div
            key={label}
            className={
              "flex items-center justify-center gap-1.5 font-mono text-[10px] tracking-[0.18em] " +
              (today === i + 1
                ? "font-bold text-foreground"
                : "text-muted-foreground/70")
            }
          >
            {today === i + 1 && (
              <span aria-hidden className="size-1 rounded-full bg-foreground" />
            )}
            {label}
          </div>
        ))}
      </div>

      <div className="relative" style={{ height: `${canvasHeight}px` }}>
        {/* Frame and hour rules live beside the columns in the SAME box, so
            their percentages and the blocks' resolve against one basis — a
            border on the columns container would shrink the blocks' basis by
            its own width and let the two drift. */}
        <div
          aria-hidden
          className="absolute top-0 right-0 left-10 border-t border-border/60"
        />
        <div
          aria-hidden
          className="absolute right-0 bottom-0 left-10 border-b border-border/60"
        />
        {hourMarks.map((h) => (
          <div key={h} aria-hidden>
            <div
              className="absolute right-0 left-10 border-t border-border/60"
              style={{ top: `${pct(h * 60)}%` }}
            />
            <span
              className="absolute left-0 w-8 -translate-y-1/2 text-right font-mono text-[9px] whitespace-nowrap text-muted-foreground/60"
              style={{ top: `${pct(h * 60)}%` }}
            >
              {h % 12 === 0 ? 12 : h % 12} {h < 12 ? "AM" : "PM"}
            </span>
          </div>
        ))}

        <div className="relative ml-10 grid h-full grid-cols-5">
          {DAY_LABELS.map((label, i) => (
            <div
              key={label}
              className={
                "relative border-l border-border/60 last:border-r " +
                (today === i + 1 ? "bg-muted/50" : "")
              }
            >
              {blocks
                .filter((b) => b.meeting.weekday === i + 1)
                .map((block) => (
                  <MeetingBlock
                    key={`${block.cls.id}-${block.meeting.startTime}`}
                    block={block}
                    pct={pct}
                  />
                ))}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function MeetingBlock({
  block,
  pct,
}: {
  block: Block;
  pct: (minutes: number) => number;
}) {
  const { cls, meeting, inSession } = block;
  const start = toMinutes(meeting.startTime);
  const end = toMinutes(meeting.endTime);
  const heightPx = ((end - start) / 60) * PX_PER_HOUR;
  const style = {
    top: `${pct(start)}%`,
    height: `${pct(end) - pct(start)}%`,
    "--accent": CLASS_ACCENTS[cls.color] ?? "var(--class-blue)",
  } as CSSProperties;

  return (
    <div
      title={`${cls.displayName} · ${formatTimeRange(meeting.startTime, meeting.endTime)}`}
      style={style}
      className={
        "absolute inset-x-1 overflow-hidden rounded-[5px] py-1 pr-1.5 pl-2.5 " +
        (inSession ? "bg-(--accent)" : "bg-(--accent)/12")
      }
    >
      <span
        aria-hidden
        className={
          "absolute inset-y-0 left-0 w-[3px] " +
          (inSession ? "bg-white/40" : "bg-(--accent)")
        }
      />
      <p
        className={
          "text-[11px] leading-[1.25] font-medium " +
          (heightPx >= 50 ? "line-clamp-2 " : "truncate ") +
          (inSession ? "text-white" : "text-(--accent)")
        }
      >
        {cls.displayName}
      </p>
      {heightPx >= 44 && (
        <p
          className={
            "mt-0.5 truncate font-mono text-[9.5px] " +
            (inSession ? "text-white/80" : "text-muted-foreground")
          }
        >
          {formatTimeRange(meeting.startTime, meeting.endTime)}
        </p>
      )}
    </div>
  );
}

/** The soonest upcoming meeting across every class (SPEC §11 next-class chip). */
function NextClassChip({ classes }: { classes: ClassInfo[] }) {
  let best: { cls: ClassInfo; next: NextMeeting } | null = null;
  for (const cls of classes) {
    const next = nextMeeting(cls.meetings);
    if (!next) continue;
    if (
      best === null ||
      next.daysUntil < best.next.daysUntil ||
      (next.daysUntil === best.next.daysUntil &&
        toMinutes(next.meeting.startTime) <
          toMinutes(best.next.meeting.startTime))
    ) {
      best = { cls, next };
    }
  }
  if (!best) return null;

  const style = {
    "--accent": CLASS_ACCENTS[best.cls.color] ?? "var(--class-blue)",
  } as CSSProperties;

  if (best.next.inSession) {
    return (
      <span
        style={style}
        className="flex min-w-0 items-center gap-2 rounded-full bg-(--accent) px-2.5 py-1"
      >
        <span className="shrink-0 font-mono text-[10px] font-medium tracking-[0.12em] text-white">
          IN SESSION
        </span>
        <span className="min-w-0 truncate text-[12px] text-white/90">
          {best.cls.displayName}
        </span>
      </span>
    );
  }
  return (
    <span style={style} className="flex min-w-0 items-baseline gap-2">
      <span className="shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
        NEXT
      </span>
      <span className="shrink-0 font-mono text-[11px] font-medium tracking-[0.06em] text-(--accent)">
        {weekdayLabel(best.next.meeting.weekday)}{" "}
        {formatTime(best.next.meeting.startTime)}
      </span>
      <span className="min-w-0 truncate text-[12.5px] text-muted-foreground">
        {best.cls.displayName}
      </span>
    </span>
  );
}

/** Days until the final exam (SPEC §11 exam countdown chips). */
function ExamChip({ cls }: { cls: ClassInfo }) {
  const start = cls.finalExamStart;
  if (!start) return null;
  const days = daysUntil(start);
  const countdown =
    days === 0 ? "FINAL TODAY" : days === 1 ? "FINAL TOMORROW" : `FINAL IN ${days} DAYS`;
  const style = {
    "--accent": CLASS_ACCENTS[cls.color] ?? "var(--class-blue)",
  } as CSSProperties;

  return (
    <span
      style={style}
      className="flex max-w-full items-baseline gap-2 rounded-md bg-(--accent)/12 px-2.5 py-1.5"
    >
      <span className="shrink-0 font-mono text-[10px] font-medium tracking-[0.12em] text-(--accent)">
        {countdown}
      </span>
      <span className="shrink-0 font-mono text-[10px] tracking-[0.12em] text-muted-foreground">
        {formatDueDate(start)}
      </span>
      <span className="min-w-0 truncate text-[12px] text-foreground/80">
        {cls.displayName}
      </span>
    </span>
  );
}
