import { useState } from "react";
import { useQuery } from "@tanstack/react-query";

import {
  listUnits,
  syncCanvas,
  useCanvasSync,
  type ClassOutcome,
  type Unit,
} from "@/lib/canvas";
import { monoAction } from "@/lib/styles";

/**
 * SPEC §5/§7.2 — how this course divides itself up.
 *
 * The four courses genuinely disagree about their own shape, so this lists
 * whatever each one actually declares rather than a structure the app imposes.
 * Numbering is the course's own order, which is real information here — these
 * are a sequence, and a reader looking for "week 7" is looking for a position.
 *
 * Only what a course declares appears. A folder is where material sits, which
 * the Materials tree above already shows; listing folders here put a second
 * numbering sequence under the course's own and labelled it as a guess.
 */
export function StructureSection({ classId }: { classId: number }) {
  const { data: units, error } = useQuery({
    queryKey: ["units", classId],
    queryFn: () => listUnits(classId),
  });
  const progress = useCanvasSync();
  const [refused, setRefused] = useState<string | null>(null);
  const running = progress !== null && !progress.done;
  const outcome = progress?.done
    ? progress.results?.find((r) => r.classId === classId)
    : undefined;

  async function start() {
    setRefused(null);
    try {
      await syncCanvas([classId]);
    } catch (e) {
      setRefused(String(e));
    }
  }

  return (
    <section className="mt-12" aria-label="Structure">
      <div className="flex items-baseline justify-between border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          STRUCTURE
        </h2>
        <button
          type="button"
          disabled={running}
          onClick={() => void start()}
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
      {/* The sync never began — one is already running. Distinct from a sync
          that started and failed, and it clears on the next attempt. */}
      {refused && (
        <p className="mt-3 font-mono text-[11px] leading-relaxed text-destructive">
          NOT STARTED — {refused}
        </p>
      )}
      {progress?.done && progress.error && (
        <p className="mt-3 font-mono text-[11px] leading-relaxed text-destructive">
          SYNC STOPPED — {progress.error}
        </p>
      )}
      {/* A class that failed inside a sync that otherwise finished. Carried in
          the same ink as a stopped sync, because it is a failure and not a
          remark about one. */}
      {outcome?.error && (
        <p className="mt-3 font-mono text-[11px] leading-relaxed text-destructive">
          THIS CLASS DID NOT SYNC — {outcome.error}
        </p>
      )}
      <SyncNotes outcome={outcome} />

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
            Nothing yet. Scan this course's syllabus from the Deadlines section
            — the weekly schedule is usually in the same document as the due
            dates. Syncing Canvas picks up any modules the course publishes,
            which not every course does.
          </p>
        ) : (
          <>
            <p className="text-[12px] text-muted-foreground">
              {provenance(units)}
            </p>
            <ol className="mt-2 space-y-0.5">
              {units.map((unit) => (
                <UnitRow key={unit.id} unit={unit} />
              ))}
            </ol>
          </>
        )}
      </div>
    </section>
  );
}

/** What a finished sync had to say about this class, beyond its counts. */
function SyncNotes({ outcome }: { outcome: ClassOutcome | undefined }) {
  if (!outcome || outcome.notes.length === 0) return null;
  return (
    <ul className="mt-3 max-w-xl space-y-1">
      {outcome.notes.map((note, index) => (
        <li
          key={`${outcome.classId}-${index}`}
          className="text-[12px] leading-relaxed text-muted-foreground"
        >
          {note}
        </li>
      ))}
    </ul>
  );
}

function UnitRow({ unit }: { unit: Unit }) {
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
    </li>
  );
}

/** One line naming where this list came from, in place of a label per row. */
function provenance(units: Unit[]): string {
  const kinds = new Set(units.map((u) => u.kind));
  const sources = new Set(units.map((u) => u.source));
  // The course's own word where every row agrees on one. A course that
  // declares two levels gets the neutral noun rather than the first row's.
  const noun = kinds.size === 1 ? [...kinds][0] : "division";
  const plural = units.length === 1 ? noun : `${noun}s`;
  const from =
    sources.size > 1
      ? "Canvas and this course's syllabus"
      : sources.has("canvas")
        ? "Canvas"
        : "this course's syllabus";
  return `${units.length} ${plural}, read from ${from}.`;
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
