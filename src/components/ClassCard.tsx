import type { CSSProperties } from "react";

import { CLASS_ACCENTS, splitUnitName, type ClassInfo } from "@/lib/classes";
import { formatPercent } from "@/lib/grades";
import {
  chip,
  chipAccent,
  chipAmber,
  errorLine,
  meta,
  washCard,
} from "@/lib/styles";
import {
  dueDayLabel,
  formatTimeRange,
  nextMeeting,
  relativeLabel,
  weekdayLabel,
} from "@/lib/schedule";

/**
 * SPEC §12 — a class card: the wash is the card, the week's topic is its
 * headline. One full-surface button opens the workspace, so nothing else on
 * the card is a target.
 */
export function ClassCard({
  info,
  onOpen,
}: {
  info: ClassInfo;
  onOpen: (cls: ClassInfo) => void;
}) {
  const next = nextMeeting(info.meetings);
  const division = info.currentUnit ? splitUnitName(info.currentUnit.name) : null;
  const style = {
    "--accent": CLASS_ACCENTS[info.color] ?? "var(--class-blue)",
  } as CSSProperties;

  return (
    <article style={style} className={washCard}>
      <button
        type="button"
        aria-label={`Open ${info.displayName}`}
        onClick={() => onOpen(info)}
        className="absolute inset-0 z-[1] cursor-pointer rounded-[inherit] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-(--accent)"
      />

      <h2 className="text-[15px] font-semibold leading-snug text-(--accent-ink)">
        {info.displayName}
      </h2>
      {next ? (
        <p className={`mt-1 flex items-center gap-2 ${meta}`}>
          <span>
            {weekdayLabel(next.meeting.weekday)}{" "}
            {formatTimeRange(next.meeting.startTime, next.meeting.endTime)}
          </span>
          {next.inSession ? (
            <span className={`${chip} bg-(--accent) text-white dark:text-background`}>
              In session
            </span>
          ) : (
            <span>· {relativeLabel(next)}</span>
          )}
        </p>
      ) : (
        <p className={`mt-1 ${meta}`}>No scheduled meetings</p>
      )}

      {/* Where the course is in its own sequence (SPEC §8.5), in its own words;
          a course that publishes no dates shows nothing here. */}
      {division && (
        <div className="mt-5">
          {division.topic === null ? (
            <p className="text-headline">{division.label}</p>
          ) : (
            <>
              <p className={meta}>{division.label}</p>
              <p className="mt-0.5 text-headline">{division.topic}</p>
            </>
          )}
        </div>
      )}

      {info.nextDeadline && (
        <p className="mt-5 text-body">
          <span className="font-medium">{info.nextDeadline.title}</span>{" "}
          <span className="text-muted-foreground">
            {dueDayLabel(info.nextDeadline.dueAt).startsWith("overdue")
              ? dueDayLabel(info.nextDeadline.dueAt)
              : `due ${dueDayLabel(info.nextDeadline.dueAt)}`}
          </span>
        </p>
      )}

      {(info.currentGrade != null ||
        info.pendingDeadlineProposals > 0 ||
        info.inboxPending > 0 ||
        info.staleGuides > 0) && (
        <div className="mt-auto flex flex-wrap items-center gap-1.5 pt-5">
          {info.currentGrade != null && (
            <span
              title="Current weighted grade over graded items"
              className={chipAccent}
            >
              Grade {formatPercent(info.currentGrade)}
            </span>
          )}
          {info.pendingDeadlineProposals > 0 && (
            <span
              title="Proposed deadlines waiting for your decision in the workspace"
              className={chipAccent}
            >
              {info.pendingDeadlineProposals === 1
                ? "1 proposed deadline"
                : `${info.pendingDeadlineProposals} proposed deadlines`}
            </span>
          )}
          {info.inboxPending > 0 && (
            <span
              title="Files waiting in the drop-to-sort inbox"
              className={chipAccent}
            >
              {info.inboxPending === 1 ? "1 to sort" : `${info.inboxPending} to sort`}
            </span>
          )}
          {info.staleGuides > 0 && (
            <span
              title="A study guide's sources changed since it was written"
              className={chipAmber}
            >
              {info.staleGuides === 1 ? "1 guide stale" : `${info.staleGuides} guides stale`}
            </span>
          )}
        </div>
      )}

      {!info.folderPresent && (
        <p className={errorLine}>Folder not found in AIBHS</p>
      )}
    </article>
  );
}
