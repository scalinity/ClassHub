import { useState, type CSSProperties, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";

import { DueChips } from "@/components/DeadlineStrip";
import { ActionLine } from "@/components/Notices";
import { syncCanvas, useCanvasSync } from "@/lib/canvas";
import { CLASS_ACCENTS, classesQuery, type ClassInfo } from "@/lib/classes";
import { undoAuditRows } from "@/lib/notices";
import { queryClient } from "@/lib/query";
import { formatDueDate, formatStamp, formatTimeRange } from "@/lib/schedule";
import {
  buttonText,
  buttonTextNeutral,
  errorLine,
  meta,
  readingText,
} from "@/lib/styles";
import { todayQuery, type Overnight, type UndoGroup } from "@/lib/today";

/** Due within three days: today and the three days after it. */
const DUE_WITHIN_DAYS = 4;

/**
 * SPEC §12 — Today. The date is the dashboard's headline, and this is its
 * docket, straight from the tables and zero tokens: each meeting today with
 * its division, what is due within three days as the strip's own chips,
 * what the last shift did with an `Undo` on whatever is still reversible,
 * the notices posted since the app was last opened with their to-dos, and
 * every decision waiting. A label in the margin names each line's kind;
 * absent lines are absent, and when nothing is on the docket the block is
 * one sentence and one action. Whether anything is due is the chips' own
 * call — a chip marked done here stays on the docket, struck through, and
 * the empty sentence takes its place only once the chips have nothing.
 */
export function Today({
  onOpen,
}: {
  onOpen: (cls: ClassInfo, scope?: string) => void;
}) {
  const { data: today, error } = useQuery(todayQuery());
  const { data: classes } = useQuery(classesQuery());
  const open = (classId: number, scope?: string) => {
    const cls = classes?.find((c) => c.id === classId);
    if (cls) onOpen(cls, scope);
  };
  if (error) {
    return <p className={`${errorLine} mt-8`}>Today didn't load: {String(error)}</p>;
  }
  if (today === undefined) return null;

  const meetings = today.meetings.map((m, index) => (
    <Row key={`${m.classId}-${m.startTime}-${index}`} label={formatTimeRange(m.startTime, m.endTime)}>
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <ClassLink name={m.className} color={m.classColor} onClick={() => open(m.classId)} />
        {m.unitName && (
          <span className="min-w-0 truncate text-body text-muted-foreground">
            {m.unitName}
            {m.week !== null && ` · Week ${m.week}`}
          </span>
        )}
        {m.prereadScope !== null && (
          <button
            type="button"
            style={accent(m.classColor)}
            title="The page written before this class"
            onClick={() => open(m.classId, m.prereadScope ?? undefined)}
            className={`${buttonText} -my-1`}
          >
            Before class
          </button>
        )}
      </div>
    </Row>
  ));

  const rest: ReactNode[] = [
    today.overnight && <OvernightRow key="overnight" overnight={today.overnight} />,
    today.notices.length > 0 && (
      <Row
        key="notices"
        label="Notices"
        title={
          today.since !== null
            ? `Posted since the app was last opened, ${formatStamp(new Date(today.since * 1000))}`
            : undefined
        }
      >
        <ul className="space-y-2">
          {today.notices.map((notice) => (
            <li key={notice.id}>
              <div className="flex flex-wrap items-baseline gap-x-2">
                <ClassLink
                  name={notice.className}
                  color={notice.classColor}
                  onClick={() => open(notice.classId)}
                />
                <span className="min-w-0 flex-1 truncate text-body">{notice.title}</span>
                <span className={`shrink-0 ${meta}`}>{formatDueDate(notice.postedAt)}</span>
              </div>
              {notice.actions.length > 0 && (
                <ul
                  style={accent(notice.classColor)}
                  className="mt-1 space-y-1"
                  aria-label={`What ${notice.title} asks`}
                >
                  {notice.actions.map((action) => (
                    <ActionLine key={action.id} action={action} />
                  ))}
                </ul>
              )}
            </li>
          ))}
          {today.noticesMore > 0 && (
            <li className={meta}>
              and {today.noticesMore === 1 ? "1 more" : `${today.noticesMore} more`} in the
              workspaces
            </li>
          )}
        </ul>
      </Row>
    ),
    today.waiting.length > 0 && (
      <Row key="waiting" label="Waiting" title="Decisions nothing has made yet">
        <ul className="space-y-1">
          {today.waiting.map((item, index) => (
            <li key={index} className="flex flex-wrap items-baseline gap-x-2 text-body">
              {item.classId !== null && item.className !== null && item.classColor !== null && (
                <ClassLink
                  name={item.className}
                  color={item.classColor}
                  onClick={() => open(item.classId as number)}
                />
              )}
              <span className="min-w-0 text-muted-foreground">{item.text}</span>
            </li>
          ))}
        </ul>
      </Row>
    ),
  ].filter(Boolean);
  const nothingElse = meetings.length === 0 && rest.length === 0;

  return (
    <section className="mt-8" aria-label="Today">
      {meetings}
      <DueChips
        within={DUE_WITHIN_DAYS}
        empty={nothingElse ? <EmptyDocket /> : null}
        wrap={(chips) => <Row label="Due">{chips}</Row>}
      />
      {rest}
    </section>
  );
}

/** Nothing on the docket: one sentence and one action. */
function EmptyDocket() {
  const progress = useCanvasSync();
  const running = progress !== null && !progress.done;
  const [refused, setRefused] = useState<string | null>(null);
  return (
    <>
      <p className={`${readingText} text-muted-foreground`}>
        Nothing on the docket — no meeting today, nothing due within three days, and
        nothing waiting on you.
      </p>
      <button
        type="button"
        disabled={running}
        onClick={() => {
          setRefused(null);
          syncCanvas().catch((e) => setRefused(String(e)));
        }}
        className={`${buttonTextNeutral} mt-2 -ml-2`}
      >
        {running ? "Syncing Canvas…" : "Sync Canvas"}
      </button>
      {refused && <p className={errorLine}>Not started: {refused}</p>}
    </>
  );
}

/** One line of the docket: its kind in the margin, its content beside. */
function Row({
  label,
  title,
  children,
}: {
  label: string;
  title?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex gap-5 border-b border-border/70 py-3 last:border-b-0">
      <span title={title} className={`w-24 shrink-0 pt-px ${meta}`}>
        {label}
      </span>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

function accent(color: string): CSSProperties {
  return { "--accent": CLASS_ACCENTS[color] ?? "var(--class-blue)" } as CSSProperties;
}

/** A class's name in its ink, opening its workspace. */
function ClassLink({
  name,
  color,
  onClick,
}: {
  name: string;
  color: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      style={accent(color)}
      onClick={onClick}
      className="shrink-0 cursor-pointer rounded-sm text-body font-medium text-(--accent-ink) transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
    >
      {name}
    </button>
  );
}

/** What the last shift did, and an `Undo` on each batch still reversible. */
function OvernightRow({ overnight }: { overnight: Overnight }) {
  const recent =
    overnight.finishedAt !== null && Date.now() / 1000 - overnight.finishedAt < 20 * 3600;
  const label = overnight.running ? "Shift" : recent ? "Overnight" : "Last shift";
  const when = overnight.finishedAt
    ? formatStamp(new Date(overnight.finishedAt * 1000))
    : "running now";
  return (
    <Row label={label} title={`The shift of ${overnight.night} · ${when}`}>
      <p className="text-body">
        {overnight.summary}
        {!recent && !overnight.running && (
          <span className="text-muted-foreground"> · {when}</span>
        )}
      </p>
      {overnight.undo.length > 0 && (
        <ul className="mt-1 space-y-0.5">
          {overnight.undo.map((group, index) => (
            <UndoLine key={index} group={group} />
          ))}
        </ul>
      )}
    </Row>
  );
}

function UndoLine({ group }: { group: UndoGroup }) {
  const [state, setState] = useState<"shown" | "undoing" | "undone" | "refused">("shown");
  const [detail, setDetail] = useState<string | null>(null);
  const undo = () => {
    setState("undoing");
    undoAuditRows(group.auditIds)
      .then((outcome) => {
        setState(outcome.undone.length > 0 ? "undone" : "refused");
        setDetail(outcome.refused.length > 0 ? outcome.refused.join(" · ") : null);
        void queryClient.invalidateQueries({ queryKey: ["today"] });
      })
      .catch((e) => {
        setState("refused");
        setDetail(String(e));
      });
  };
  return (
    <li className="flex flex-wrap items-center gap-x-2 text-body">
      <span
        className={
          state === "undone" ? "text-muted-foreground line-through" : "text-muted-foreground"
        }
      >
        {group.text}
      </span>
      {state === "shown" && (
        <button
          type="button"
          onClick={undo}
          className={`${buttonTextNeutral} -my-1 font-semibold text-foreground`}
        >
          Undo
        </button>
      )}
      {state === "undoing" && <span className={meta}>Undoing…</span>}
      {state === "undone" && <span className={meta}>undone</span>}
      {detail && (
        <span className="text-fine text-destructive">
          {state === "refused" ? "Not undone: " : "Partly: "}
          {detail}
        </span>
      )}
    </li>
  );
}
