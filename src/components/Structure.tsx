import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";

import {
  listUnits,
  syncCanvas,
  useCanvasSync,
  type ClassOutcome,
  type Unit,
} from "@/lib/canvas";
import { unitScope, type GuideInfo } from "@/lib/guides";
import { listLectureContributions } from "@/lib/lectures";
import { monoAction } from "@/lib/styles";

/** What a division's guide needs from the workspace to be triggered and read. */
export interface UnitGuideControls {
  guides: ReadonlyMap<string, GuideInfo>;
  activeScopes: ReadonlySet<string>;
  onSynthesize: (unitId: number) => Promise<unknown>;
  onView: (scope: string) => void;
}

/**
 * SPEC §5/§7.2 — how this course divides itself up, and §8.1 — the guide for
 * each division.
 *
 * The four courses genuinely disagree about their own shape, so this lists
 * whatever each one actually declares rather than a structure the app imposes.
 * Numbering is the course's own order, which is real information here — these
 * are a sequence, and a reader looking for "week 7" is looking for a position.
 *
 * Only what a course declares appears. A folder is where material sits, which
 * the Materials tree above already shows; listing folders here put a second
 * numbering sequence under the course's own and labelled it as a guess.
 *
 * This is also where a lecture lands. A session filed under `Weeks/` feeds the
 * division that week belongs to (SPEC §8.5), so the count of lectures behind a
 * division is what says whether its guide has anything spoken to build from.
 */
export function StructureSection({
  classId,
  currentUnitId,
  controls,
}: {
  classId: number;
  /** The division the course is in today (SPEC §8.5) — null where the course
   *  published no dates. */
  currentUnitId: number | null;
  controls: UnitGuideControls;
}) {
  const { data: units, error } = useQuery({
    queryKey: ["units", classId],
    queryFn: () => listUnits(classId),
  });
  const { data: contributions } = useQuery({
    queryKey: ["contributions", classId],
    queryFn: () => listLectureContributions(classId),
    placeholderData: (prev) => prev,
  });
  const distilledPerUnit = new Map<number, number>();
  for (const c of contributions ?? []) {
    if (c.distilled) {
      distilledPerUnit.set(c.unitId, (distilledPerUnit.get(c.unitId) ?? 0) + 1);
    }
  }
  const progress = useCanvasSync();
  const [refused, setRefused] = useState<string | null>(null);
  const [synthError, setSynthError] = useState<string | null>(null);
  const running = progress !== null && !progress.done;
  const synthesize = (unitId: number) => {
    setSynthError(null);
    controls.onSynthesize(unitId).catch((e) => setSynthError(String(e)));
  };
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
      {synthError && (
        <p className="mt-3 font-mono text-[11px] leading-relaxed text-destructive">
          NO GUIDE — {synthError}
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
            <p className="max-w-xl text-[12px] leading-relaxed text-muted-foreground">
              {provenance(units)}
              {/* Without this, seventeen rows with no action on any of them
                  read as a list the app does nothing with. */}
              {distilledPerUnit.size === 0 &&
                " Each one can have its own study guide, built from the lectures filed under it — add a lecture and distill it to start one."}
            </p>
            <ol className="mt-2 space-y-0.5">
              {units.map((unit) => (
                <UnitRow
                  key={unit.id}
                  unit={unit}
                  current={unit.id === currentUnitId}
                  distilled={distilledPerUnit.get(unit.id) ?? 0}
                  controls={controls}
                  onSynthesize={synthesize}
                />
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

function UnitRow({
  unit,
  current,
  distilled,
  controls,
  onSynthesize,
}: {
  unit: Unit;
  /** This is the division the course is in today. */
  current: boolean;
  distilled: number;
  controls: UnitGuideControls;
  onSynthesize: (unitId: number) => void;
}) {
  const scope = unitScope(unit.name);
  const guide = controls.guides.get(scope);
  // A division with no folder and no distilled lecture has nothing to build a
  // guide from, so the action stays off the row rather than offering a button
  // that can only refuse. A guide that already exists is shown regardless: a
  // lecture refiled out of this week leaves its guide behind and stale, and
  // that is exactly what the row must not hide — viewable, without an action
  // the backend would turn down.
  const canBuild = distilled > 0 || unit.relPath !== null;
  const hasGuideCluster = canBuild || guide !== undefined;

  return (
    <li
      aria-current={current ? "true" : undefined}
      className="group flex h-8 items-center gap-3 rounded-md px-2 transition-colors hover:bg-muted/60"
    >
      <span
        aria-hidden
        className="w-5 shrink-0 text-right font-mono text-[10px] tabular-nums text-muted-foreground/60"
      >
        {unit.ordinal}
      </span>
      <span className="min-w-0 flex-1 truncate text-[13px]">{unit.name}</span>
      {/* The list marks where the course is; it does not scroll there. A
          still dot, because nothing is running — the pulse means a job. */}
      {current && (
        <span className="flex shrink-0 items-center gap-1.5 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
          <span aria-hidden className="size-1.5 rounded-full bg-(--accent)" />
          NOW
        </span>
      )}
      {distilled > 0 && (
        <span
          title={
            distilled === 1
              ? "One distilled lecture feeds this guide"
              : `${distilled} distilled lectures feed this guide`
          }
          className="shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70"
        >
          {distilled === 1 ? "1 LECTURE" : `${distilled} LECTURES`}
        </span>
      )}
      {hasGuideCluster && (
        <UnitGuideCluster
          scope={scope}
          unitId={unit.id}
          unitName={unit.name}
          guide={guide}
          canBuild={canBuild}
          controls={controls}
          onSynthesize={onSynthesize}
        />
      )}
      {unit.startsOn && (
        <span className="shrink-0 font-mono text-[10px] tracking-[0.1em] text-muted-foreground">
          {formatUnitDate(unit.startsOn)}
        </span>
      )}
    </li>
  );
}

/**
 * SPEC §8.1 — a division's guide, in the same words the Materials tree uses for
 * a folder's: synthesis is manual, staleness is always visible, and the
 * token-costing action stays quiet until the guide has actually gone stale.
 */
function UnitGuideCluster({
  scope,
  unitId,
  unitName,
  guide,
  canBuild,
  controls,
  onSynthesize,
}: {
  scope: string;
  unitId: number;
  unitName: string;
  guide: GuideInfo | undefined;
  /** Whether the division has anything to build a guide from right now. */
  canBuild: boolean;
  controls: UnitGuideControls;
  onSynthesize: (unitId: number) => void;
}) {
  if (controls.activeScopes.has(scope)) {
    return (
      <span className="flex shrink-0 items-center gap-1.5 px-1.5 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
        <span
          aria-hidden
          className="size-1.5 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
        />
        SYNTHESIZING…
      </span>
    );
  }

  if (!guide) {
    return (
      <button
        type="button"
        onClick={() => onSynthesize(unitId)}
        className={`${monoAction} text-muted-foreground opacity-0 transition-opacity hover:bg-(--accent)/12 hover:text-(--accent) focus-visible:opacity-100 group-focus-within:opacity-100 group-hover:opacity-100`}
      >
        SYNTHESIZE GUIDE
      </button>
    );
  }

  return (
    <span className="flex shrink-0 items-center gap-0.5">
      {guide.stale && !canBuild ? (
        // Stale with nothing left to rebuild from: said, not offered.
        <span
          title="Its sources are gone — refile a lecture here to rebuild it"
          className="px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-class-amber"
        >
          STALE
        </span>
      ) : guide.stale ? (
        <button
          type="button"
          title="Sources changed since this guide was generated"
          onClick={() => onSynthesize(unitId)}
          className={`${monoAction} bg-class-amber/12 text-class-amber hover:bg-class-amber/20`}
        >
          STALE — RESYNTHESIZE
        </button>
      ) : !canBuild ? null : (
        <button
          type="button"
          title="Resynthesize guide"
          aria-label={`Resynthesize the ${unitName} guide`}
          onClick={() => onSynthesize(unitId)}
          className="cursor-pointer rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-muted hover:text-foreground focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-(--accent) group-focus-within:opacity-100 group-hover:opacity-100"
        >
          <RefreshCw size={12} aria-hidden />
        </button>
      )}
      <button
        type="button"
        onClick={() => controls.onView(scope)}
        className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
      >
        VIEW GUIDE
      </button>
    </span>
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
