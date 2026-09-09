import type { Unit } from "@/lib/canvas";
import { splitUnitName } from "@/lib/classes";
import type { Deadline } from "@/lib/deadlines";
import { unitScope, type GuideInfo } from "@/lib/guides";
import type { Contribution } from "@/lib/lectures";
import { formatMeetingDay } from "@/lib/schedule";

/** One division's state, read off the queries the workspace already holds. */
interface Segment {
  unit: Unit;
  label: string;
  filed: number;
  distilled: number;
  guide: "none" | "fresh" | "stale";
  assessments: Deadline[];
  current: boolean;
}

/**
 * SPEC §12 — the semester, per class: each division as a segment in the
 * course's own order, its state in form — a lecture filed, a note
 * distilled, a guide fresh or stale, a quiz or exam due in it — and today's
 * mark. Two lines a segment: the top is the lectures, ink when a transcript
 * is filed and the class colour once one is distilled; the bottom is the
 * guide, the colour when fresh and amber when stale. A ring above the
 * number marks an assessment. The title carries the words.
 */
export function SemesterStrip({
  units,
  contributions,
  guides,
  deadlines,
  currentUnitId,
  currentWeek,
}: {
  units: Unit[];
  contributions: Contribution[];
  guides: ReadonlyMap<string, GuideInfo>;
  /** The class's own deadlines. */
  deadlines: Deadline[];
  currentUnitId: number | null;
  /** The week inside the current division, where the reading is a filed
   *  lecture's (SPEC §8.5). */
  currentWeek: number | null;
}) {
  if (units.length === 0) return null;
  const segments = units.map((unit, index): Segment => {
    const mapped = contributions.filter((c) => c.unitId === unit.id);
    const guide = guides.get(unitScope(unit.id));
    // A quiz or exam falls in the dated division whose span holds its day:
    // from its start to the next dated one's.
    const starts = unit.startsOn;
    const next = units.slice(index + 1).find((u) => u.startsOn !== null)?.startsOn ?? null;
    const assessments =
      starts === null
        ? []
        : deadlines.filter(
            (d) =>
              (d.kind === "quiz" || d.kind === "exam") &&
              d.dueAt.slice(0, 10) >= starts &&
              (next === null || d.dueAt.slice(0, 10) < next),
          );
    return {
      unit,
      label: segmentLabel(unit),
      filed: mapped.length,
      distilled: mapped.filter((c) => c.distilled).length,
      guide: guide === undefined ? "none" : guide.stale ? "stale" : "fresh",
      assessments,
      current: unit.id === currentUnitId,
    };
  });

  return (
    <div className="mt-6" aria-label="The semester">
      <ol className="flex gap-1">
        {segments.map((s) => (
          <li
            key={s.unit.id}
            title={segmentTitle(s, currentWeek)}
            aria-current={s.current ? "true" : undefined}
            className="min-w-0 flex-1"
          >
            <div className="flex h-2.5 items-end justify-center">
              {s.assessments.length > 0 && (
                <span
                  aria-hidden
                  className="mb-0.5 size-1.5 rounded-full border border-(--accent-ink)"
                />
              )}
            </div>
            <div
              aria-hidden
              className={
                "h-[3px] rounded-sm " +
                (s.distilled > 0
                  ? "bg-(--accent)"
                  : s.filed > 0
                    ? "bg-foreground/45"
                    : "bg-muted-foreground/20")
              }
            />
            {/* Stale is amber and dashed: amber alone is the class colour
                on one of the four, so the form carries it there. */}
            <div
              aria-hidden
              className={
                "mt-0.5 h-[3px] rounded-sm " +
                (s.guide === "fresh"
                  ? "bg-(--accent)"
                  : s.guide === "stale"
                    ? "border-t-[3px] border-dashed border-class-amber"
                    : "bg-transparent")
              }
            />
            <p
              className={
                "mt-1 truncate text-center text-fine tabular-nums " +
                (s.current ? "font-semibold text-(--accent-ink)" : "text-muted-foreground")
              }
            >
              {s.label}
              {s.current && currentWeek !== null && ` · wk ${currentWeek}`}
            </p>
          </li>
        ))}
      </ol>
      <p className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-fine text-muted-foreground">
        <Key swatch="bg-foreground/45" word="lecture filed" />
        <Key swatch="bg-(--accent)" word="distilled, or a fresh guide" />
        <Key swatch="border-t-[3px] border-dashed border-class-amber" word="guide stale" />
        <span className="flex items-center gap-1.5">
          <span aria-hidden className="size-1.5 rounded-full border border-(--accent-ink)" />
          quiz or exam
        </span>
      </p>
    </div>
  );
}

function Key({ swatch, word }: { swatch: string; word: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <span aria-hidden className={`h-[3px] w-4 rounded-sm ${swatch}`} />
      {word}
    </span>
  );
}

/** `3` for a week, `1–8` for a Part over a range, the label's word otherwise. */
function segmentLabel(unit: Unit): string {
  if (unit.kind !== "week" && unit.firstWeek !== null && unit.lastWeek !== null) {
    return `${unit.firstWeek}–${unit.lastWeek}`;
  }
  if (unit.number !== null) return String(unit.number);
  return splitUnitName(unit.name).label;
}

function segmentTitle(s: Segment, currentWeek: number | null): string {
  const parts = [s.unit.name];
  if (s.unit.startsOn) parts.push(formatMeetingDay(s.unit.startsOn));
  if (s.current) parts.push(currentWeek !== null ? `now, week ${currentWeek}` : "now");
  parts.push(
    s.filed === 0
      ? "no lecture filed"
      : `${s.filed} ${s.filed === 1 ? "lecture" : "lectures"} filed, ${s.distilled} distilled`,
  );
  parts.push(s.guide === "none" ? "no guide" : `guide ${s.guide}`);
  for (const d of s.assessments) {
    parts.push(`${d.title} due ${formatMeetingDay(d.dueAt.slice(0, 10))}`);
  }
  return parts.join(" · ");
}
