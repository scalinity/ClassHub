import type { CSSProperties } from "react";

import { Card } from "@/components/ui/card";
import type { ClassInfo } from "@/lib/classes";
import {
  formatTimeRange,
  nextMeeting,
  relativeLabel,
  weekdayLabel,
} from "@/lib/schedule";

const ACCENTS: Record<string, string> = {
  blue: "var(--class-blue)",
  orange: "var(--class-orange)",
  green: "var(--class-green)",
  amber: "var(--class-amber)",
};

const WEEKDAY_LETTERS = ["M", "T", "W", "T", "F"];

export function ClassCard({
  info,
  onOpen,
}: {
  info: ClassInfo;
  onOpen: (cls: ClassInfo) => void;
}) {
  const next = nextMeeting(info.meetings);
  const meetingDays = new Set(info.meetings.map((m) => m.weekday));
  const style = {
    "--accent": ACCENTS[info.color] ?? "var(--class-blue)",
  } as CSSProperties;

  return (
    <Card
      style={style}
      className="relative gap-0 overflow-hidden p-6 transition-shadow duration-200 hover:shadow-md"
    >
      <button
        type="button"
        aria-label={`Open ${info.displayName}`}
        onClick={() => onOpen(info)}
        className="absolute inset-0 z-[1] cursor-pointer rounded-[inherit] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-(--accent)"
      />
      <span
        aria-hidden
        className="absolute inset-y-0 left-0 w-[3px] bg-(--accent) transition-[width] duration-200 group-hover/card:w-[6px]"
      />

      <div className="flex items-start justify-between gap-4">
        <h2 className="text-[17px] font-semibold leading-snug tracking-tight">
          {info.displayName}
        </h2>
        <div
          aria-label={`Meets on ${info.meetings.map((m) => weekdayLabel(m.weekday)).join(", ")}`}
          className="flex shrink-0 gap-1.5 pt-1.5 font-mono text-[10px] leading-none"
        >
          {WEEKDAY_LETTERS.map((letter, i) => (
            <span
              key={i}
              className={
                meetingDays.has(i + 1)
                  ? "font-bold text-(--accent)"
                  : "text-muted-foreground/40"
              }
            >
              {letter}
            </span>
          ))}
        </div>
      </div>

      <p className="mt-1 text-[13px] text-muted-foreground">{info.instructors}</p>

      <div className="mt-6 font-mono text-[12.5px]">
        <div className="flex items-center justify-between gap-3">
          {next ? (
            <p className="font-medium">
              {weekdayLabel(next.meeting.weekday)}{" "}
              {formatTimeRange(next.meeting.startTime, next.meeting.endTime)}
            </p>
          ) : (
            <p className="text-muted-foreground">NO SCHEDULED MEETINGS</p>
          )}
          {next && (
            <span
              className={
                "shrink-0 rounded-full px-2.5 py-1 text-[10px] font-medium tracking-[0.12em] " +
                (next.inSession
                  ? "bg-(--accent) text-white"
                  : "bg-(--accent)/12 text-(--accent)")
              }
            >
              {relativeLabel(next)}
            </span>
          )}
        </div>
        <p className="mt-1.5 text-muted-foreground">
          {info.room} · {info.credits} CR
        </p>
      </div>

      {!info.folderPresent && (
        <p className="mt-3 font-mono text-[10px] tracking-[0.14em] text-destructive">
          FOLDER NOT FOUND IN AIBHS
        </p>
      )}
    </Card>
  );
}
