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
import { PracticeAction } from "@/components/PracticeAction";
import { SectionHeading } from "@/components/SectionHeading";
import { deltaLabel, deltaTitle, unitScope, type GuideInfo } from "@/lib/guides";
import { listLectureContributions } from "@/lib/lectures";
import {
  buttonChip,
  buttonIcon,
  buttonText,
  buttonTextMuted,
  chipAmber,
  errorLine,
  meta,
  pulseDot,
  readingText,
  row,
  statusLine,
} from "@/lib/styles";
import { sentence } from "@/lib/utils";

/** What a division's guide needs from the workspace to be triggered and read. */
export interface UnitGuideControls {
  guides: ReadonlyMap<string, GuideInfo>;
  activeScopes: ReadonlySet<string>;
  /** Scopes with a queued/running practice job (SPEC §8.3). */
  activePracticeScopes: ReadonlySet<string>;
  onSynthesize: (unitId: number) => Promise<unknown>;
  /** Write an exam for the scope, focused on the typed topics when any. */
  onPractice: (scope: string, focus: string | null) => Promise<unknown>;
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
 * the Materials tree already shows; listing folders here put a second
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
  // A job the backend turned down — a guide or a practice exam — as the line
  // to show, since the two refusals read differently.
  const [jobError, setJobError] = useState<string | null>(null);
  const running = progress !== null && !progress.done;
  const synthesize = (unitId: number) => {
    setJobError(null);
    controls
      .onSynthesize(unitId)
      .catch((e) => setJobError(`No guide: ${String(e)}`));
  };
  const practice = (scope: string, focus: string | null) => {
    setJobError(null);
    controls
      .onPractice(scope, focus)
      .catch((e) => setJobError(`No practice exam: ${String(e)}`));
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
    <section id="structure" className="mt-14 scroll-mt-20" aria-label="Structure">
      <SectionHeading
        title="Structure"
        actions={
          <button
            type="button"
            disabled={running}
            onClick={() => void start()}
            className={buttonText}
          >
            {running ? "Syncing…" : "Sync Canvas"}
          </button>
        }
      />

      {running && (
        <p className={`mt-3 ${statusLine}`}>
          <span aria-hidden className={pulseDot} />
          {sentence(progress.stage)}
        </p>
      )}
      {/* The sync never began — one is already running. Distinct from a sync
          that started and failed, and it clears on the next attempt. */}
      {refused && <p className={errorLine}>Not started: {refused}</p>}
      {progress?.done && progress.error && (
        <p className={errorLine}>Sync stopped: {progress.error}</p>
      )}
      {/* A class that failed inside a sync that otherwise finished. Carried in
          the same ink as a stopped sync, because it is a failure and not a
          remark about one. */}
      {outcome?.error && (
        <p className={errorLine}>This class did not sync: {outcome.error}</p>
      )}
      {jobError && <p className={errorLine}>{jobError}</p>}
      <SyncNotes outcome={outcome} />

      <div className="mt-3">
        {error ? (
          <p className={errorLine}>Couldn't read the structure: {String(error)}</p>
        ) : units === undefined ? (
          <p className="py-2 text-body text-muted-foreground">Loading…</p>
        ) : units.length === 0 ? (
          <p className={`max-w-xl py-2 ${readingText} text-muted-foreground`}>
            Nothing yet. Scan this course's syllabus from the Deadlines section
            — the weekly schedule is usually in the same document as the due
            dates. Syncing Canvas picks up any modules the course publishes,
            which not every course does.
          </p>
        ) : (
          <>
            <p className="max-w-xl text-body text-muted-foreground">
              {provenance(units)}
              {/* Without this, seventeen rows with no action on any of them
                  read as a list the app does nothing with. */}
              {distilledPerUnit.size === 0 &&
                " Each one can have its own study guide, built from the lectures filed under it — add a lecture and distill it to start one."}
            </p>
            <ol className="mt-3">
              {units.map((unit) => (
                <UnitRow
                  key={unit.id}
                  unit={unit}
                  current={unit.id === currentUnitId}
                  distilled={distilledPerUnit.get(unit.id) ?? 0}
                  controls={controls}
                  onSynthesize={synthesize}
                  onPractice={practice}
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
          className="text-body text-muted-foreground"
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
  onPractice,
}: {
  unit: Unit;
  /** This is the division the course is in today. */
  current: boolean;
  distilled: number;
  controls: UnitGuideControls;
  onSynthesize: (unitId: number) => void;
  onPractice: (scope: string, focus: string | null) => void;
}) {
  const scope = unitScope(unit.id);
  const guide = controls.guides.get(scope);
  // A division with nothing filed under its folder or its weeks and no
  // distilled lecture has nothing to build a guide from, so the action stays
  // off the row rather than offering a button that can only refuse. A guide
  // that already exists is shown regardless: a lecture refiled out of this
  // week leaves its guide behind and stale, and that is exactly what the row
  // must not hide — viewable, without an action the backend would turn down.
  const canBuild = distilled > 0 || (unit.materials ?? 0) > 0;
  const hasGuideCluster =
    canBuild || guide !== undefined || controls.activePracticeScopes.has(scope);

  return (
    <li aria-current={current ? "true" : undefined} className={row}>
      <span
        aria-hidden
        className="w-6 shrink-0 text-right text-meta tabular-nums text-muted-foreground/70"
      >
        {unit.ordinal}
      </span>
      <span className="min-w-0 flex-1 truncate text-body">{unit.name}</span>
      {/* The list marks where the course is; it does not scroll there. A
          still dot, because nothing is running — the pulse means a job. */}
      {current && (
        <span className="flex shrink-0 items-center gap-1.5 text-fine font-medium text-(--accent-ink)">
          <span aria-hidden className="size-1.5 rounded-full bg-(--accent)" />
          now
        </span>
      )}
      {distilled > 0 && (
        <span
          title={
            distilled === 1
              ? "One distilled lecture feeds this guide"
              : `${distilled} distilled lectures feed this guide`
          }
          className="shrink-0 text-fine text-muted-foreground"
        >
          {distilled === 1 ? "1 lecture" : `${distilled} lectures`}
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
          onPractice={onPractice}
        />
      )}
      {unit.startsOn && (
        <span className={`w-12 shrink-0 text-right ${meta}`}>
          {formatUnitDate(unit.startsOn)}
        </span>
      )}
    </li>
  );
}

/**
 * SPEC §8.1 — a division's guide, in the same words the Materials tree uses for
 * a folder's: writing is manual, staleness is always visible, and the
 * token-costing action stays quiet until the guide has actually gone stale.
 * The practice exam (SPEC §8.3) draws on the same sources as the guide, so it
 * is offered exactly when a guide could be built.
 */
function UnitGuideCluster({
  scope,
  unitId,
  unitName,
  guide,
  canBuild,
  controls,
  onSynthesize,
  onPractice,
}: {
  scope: string;
  unitId: number;
  unitName: string;
  guide: GuideInfo | undefined;
  /** Whether the division has anything to build a guide from right now. */
  canBuild: boolean;
  controls: UnitGuideControls;
  onSynthesize: (unitId: number) => void;
  onPractice: (scope: string, focus: string | null) => void;
}) {
  // The button needs sources to start from; the pulse only needs the job.
  // An exam already being written for a division that has since lost its
  // sources is still being written, and the row should say so.
  const practiceActive = controls.activePracticeScopes.has(scope);
  const practice =
    practiceActive || canBuild ? (
      <PracticeAction
        active={practiceActive}
        onSelect={(focus) => onPractice(scope, focus)}
      />
    ) : null;

  if (controls.activeScopes.has(scope)) {
    return (
      <span className="flex shrink-0 items-center gap-0.5">
        <span className={`px-2 ${statusLine}`}>
          <span aria-hidden className={pulseDot} />
          Writing the guide…
        </span>
        {practice}
      </span>
    );
  }

  if (!guide) {
    return (
      <span className="flex shrink-0 items-center gap-0.5">
        <button
          type="button"
          onClick={() => onSynthesize(unitId)}
          className={`${buttonTextMuted}`}
        >
          Write guide
        </button>
        {practice}
      </span>
    );
  }

  return (
    <span className="flex shrink-0 items-center gap-0.5">
      {guide.stale && !canBuild ? (
        // Stale with nothing left to rebuild from: said, not offered.
        <span
          title="Its sources are gone — refile a lecture here to rebuild it"
          className={chipAmber}
        >
          stale
        </span>
      ) : guide.stale ? (
        <button
          type="button"
          title={deltaTitle(guide.diff)}
          onClick={() => onSynthesize(unitId)}
          className={`${buttonChip} bg-class-amber/12 text-class-amber hover:bg-class-amber/20`}
        >
          Rewrite · {deltaLabel(guide.diff)}
        </button>
      ) : !canBuild ? null : (
        <button
          type="button"
          title="Rewrite the guide"
          aria-label={`Resynthesize the ${unitName} guide`}
          onClick={() => onSynthesize(unitId)}
          className={`${buttonIcon}`}
        >
          <RefreshCw size={12} aria-hidden />
        </button>
      )}
      {practice}
      <button
        type="button"
        onClick={() => controls.onView(scope)}
        className={buttonText}
      >
        Read guide
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

/** `2026-09-03` → `Sep 3`. Parsed as parts, never through `new Date(iso)` —
 *  that reads a bare date as UTC and renders the day before in this zone. */
function formatUnitDate(iso: string): string {
  const [year, month, day] = iso.split("-").map(Number);
  if (!year || !month || !day) return "";
  // Never the year: the list is one term's and the column is 3rem wide.
  return new Date(year, month - 1, day).toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
  });
}
