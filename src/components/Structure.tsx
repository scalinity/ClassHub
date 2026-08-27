import { useQuery } from "@tanstack/react-query";

import { listUnits, syncCanvas, useCanvasSync, type Unit } from "@/lib/canvas";
import { monoAction } from "@/lib/styles";

/**
 * SPEC §5/§7.2 — how this course divides itself up.
 *
 * The four courses genuinely disagree about their own shape, so this lists
 * whatever each one actually declares rather than a structure the app imposes.
 * Numbering is the course's own order, which is real information here — these
 * are a sequence, and a reader looking for "week 7" is looking for a position.
 *
 * A division inferred from a folder is labelled as such. That is the one
 * distinction worth surfacing: "Module 1 exists because the syllabus says so"
 * and "Module 1 exists because a folder is called that" look identical on a
 * list, and only the first is the course speaking.
 */
export function StructureSection({
  classId,
  className,
}: {
  classId: number;
  className: string;
}) {
  const { data: units, error } = useQuery({
    queryKey: ["units", classId],
    queryFn: () => listUnits(classId),
  });
  const progress = useCanvasSync();
  const running = progress !== null && !progress.done;

  return (
    <section className="mt-12" aria-label="Structure">
      <div className="flex items-baseline justify-between border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          STRUCTURE
        </h2>
        <button
          type="button"
          disabled={running}
          onClick={() => void syncCanvas([classId])}
          className={`${monoAction} text-(--accent) hover:bg-(--accent)/12 disabled:pointer-events-none disabled:opacity-40`}
        >
          {running ? "SYNCING…" : "SYNC CANVAS"}
        </button>
      </div>

      {running && (
        <p className="mt-3 flex items-center gap-2 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
          <span
            aria-hidden
            className="size-1.5 shrink-0 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
          />
          {progress.stage.toUpperCase()}
        </p>
      )}
      {progress?.done && progress.error && (
        <p className="mt-3 font-mono text-[11px] leading-relaxed text-destructive">
          SYNC STOPPED — {progress.error}
        </p>
      )}
      {progress?.done && classNote(progress.results, classId) && (
        <p className="mt-3 text-[12px] leading-relaxed text-muted-foreground">
          {classNote(progress.results, classId)}
        </p>
      )}

      <div className="mt-3">
        {error ? (
          <p className="py-2 font-mono text-[11px] text-destructive">
            COULD NOT READ THE STRUCTURE — {String(error)}
          </p>
        ) : units === undefined ? (
          <p className="py-2 font-mono text-[11px] text-muted-foreground">
            LOADING…
          </p>
        ) : units.length === 0 ? (
          <p className="max-w-xl py-2 text-[13px] leading-relaxed text-muted-foreground">
            Nothing yet. Sync Canvas for this course's published modules, or
            scan its syllabus from the Deadlines section — the weekly schedule
            is usually in the same document as the due dates.
          </p>
        ) : (
          <>
            <p className="text-[12px] text-muted-foreground">
              {provenance(units)}
            </p>
            <ol className="mt-2 space-y-0.5">
              {units.map((unit) => (
                <UnitRow key={unit.id} unit={unit} className={className} />
              ))}
            </ol>
          </>
        )}
      </div>
    </section>
  );
}

function UnitRow({ unit, className }: { unit: Unit; className: string }) {
  const inferred = unit.source === "folder";
  return (
    <li className="flex h-8 items-center gap-3 rounded-md px-2 transition-colors hover:bg-muted/60">
      <span
        aria-hidden
        className="w-5 shrink-0 text-right font-mono text-[10px] tabular-nums text-muted-foreground/60"
      >
        {unit.ordinal}
      </span>
      <span className="min-w-0 flex-1 truncate text-[13px]">{unit.name}</span>
      {unit.startsOn && (
        <span className="shrink-0 font-mono text-[10px] tracking-[0.1em] text-muted-foreground">
          {formatUnitDate(unit.startsOn)}
        </span>
      )}
      {/* Only the inferred ones are flagged. Where every row shares a source,
          repeating it down the whole list says nothing the line above the list
          has not already said — and it buries the one row that differs. */}
      {inferred && (
        <span
          title={`A folder in ${className} is named this. The course itself does not declare ${unit.name} as one of its divisions.`}
          className="shrink-0 font-mono text-[9.5px] italic tracking-[0.12em] text-muted-foreground/50"
        >
          INFERRED
        </span>
      )}
    </li>
  );
}

/** One line naming where this list came from, in place of a label per row. */
function provenance(units: Unit[]): string {
  const declared = units.filter((u) => u.source !== "folder");
  const inferred = units.length - declared.length;
  const inferredNote =
    inferred === 0
      ? ""
      : ` · ${inferred} more inferred from folder${inferred === 1 ? "" : "s"}`;

  if (declared.length === 0) {
    return `Nothing declared yet — these ${units.length === 1 ? "is a folder" : "are folders"} in the class folder, not the course's own divisions.`;
  }
  const from =
    declared[0].source === "canvas" ? "Canvas" : "this course's syllabus";
  const noun = declared[0].kind === "part" ? "part" : declared[0].kind;
  const plural = declared.length === 1 ? noun : `${noun}s`;
  return `${declared.length} ${plural}, read from ${from}${inferredNote}.`;
}

/** `2026-09-03` → `SEP 3`. Parsed as parts, never through `new Date(iso)` —
 *  that reads a bare date as UTC and renders the day before in this zone. */
function formatUnitDate(iso: string): string {
  const [year, month, day] = iso.split("-").map(Number);
  if (!year || !month || !day) return "";
  return new Date(year, month - 1, day)
    .toLocaleDateString("en-US", { month: "short", day: "numeric" })
    .toUpperCase();
}

/** The one line from a finished sync that belongs to this class. */
function classNote(
  results: { classId: number; notes: string[]; error: string | null }[] | undefined,
  classId: number,
): string | null {
  const mine = results?.find((r) => r.classId === classId);
  if (!mine) return null;
  if (mine.error) return mine.error;
  return mine.notes[0] ?? null;
}
