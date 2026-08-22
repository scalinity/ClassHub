import { Fragment, useState } from "react";
import { RefreshCw } from "lucide-react";

import {
  formatGeneratedAt,
  resumeMasterGuide,
  synthesizeMaster,
  type GuideInfo,
} from "@/lib/guides";
import { formatElapsed, useJobs, type ProgressEvent } from "@/lib/jobs";

const monoAction =
  "shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] transition-colors focus-visible:outline-2 focus-visible:outline-(--accent)";

/**
 * SPEC §8.2 long-job UX: the run's real phases, derived from the stream.
 * ORIENT until the first source read; READ while extracts stream in; COMPOSE
 * once Write input starts streaming (`phase` events carry live byte counts);
 * VERIFY when the model greps/re-reads after writing.
 */
const PHASES = ["ORIENT", "READ", "COMPOSE", "VERIFY"] as const;

function derivePhase(events: readonly ProgressEvent[]): {
  index: number;
  detail: string;
} {
  let reads = 0;
  let composing = false;
  let verifying = false;
  let kb: number | null = null;
  for (const e of events) {
    if (e.kind === "phase") {
      composing = true;
      verifying = false;
      const m = /~(\d+) KB/.exec(e.text);
      if (m) kb = Number(m[1]);
    } else if (e.kind === "tool") {
      const tool = e.text.split(" · ")[0];
      if (tool === "Write" || tool === "Edit") {
        composing = true;
        verifying = false;
      } else if (tool === "Read" || tool === "Glob" || tool === "Grep") {
        if (composing) verifying = true;
        else reads += 1;
      }
    }
  }
  if (verifying) return { index: 3, detail: "VERIFYING OUTPUT" };
  if (composing) {
    return {
      index: 2,
      detail: kb === null ? "COMPOSING GUIDE" : `COMPOSING · ~${kb} KB WRITTEN`,
    };
  }
  if (reads > 0) {
    return {
      index: 1,
      detail: `READING SOURCES · ${reads} ${reads === 1 ? "READ" : "READS"}`,
    };
  }
  return { index: 0, detail: "ORIENTING" };
}

function PhaseRail({ current }: { current: number }) {
  return (
    <span
      aria-label={`Phase ${current + 1} of ${PHASES.length}: ${PHASES[current]}`}
      className="flex shrink-0 items-center gap-1.5 font-mono text-[10px] tracking-[0.14em]"
    >
      {PHASES.map((phase, i) => (
        <Fragment key={phase}>
          {i > 0 && (
            <span
              aria-hidden
              className={
                "h-px w-3 " + (i <= current ? "bg-(--accent)/50" : "bg-border")
              }
            />
          )}
          <span
            className={
              i === current
                ? "font-bold text-(--accent)"
                : i < current
                  ? "text-(--accent)/60"
                  : "text-muted-foreground/40"
            }
          >
            {phase}
          </span>
        </Fragment>
      ))}
    </span>
  );
}

/**
 * SPEC §8.2: the class-level semester-master strip — manual trigger, phased
 * progress with elapsed time while the exclusive job runs, and Resume on a
 * failed run (re-invokes the same claude session, SPEC §6).
 */
export function MasterGuideStrip({
  classId,
  guide,
  onView,
}: {
  classId: number;
  guide: GuideInfo | undefined;
  onView: () => void;
}) {
  const { jobs, output, nowSec } = useJobs();
  const [error, setError] = useState<string | null>(null);

  const masterJobs = jobs.filter(
    (j) => j.kind === "master_guide" && j.classId === classId,
  );
  const active =
    masterJobs.find((j) => j.status === "running" || j.status === "queued") ??
    null;
  const failed =
    !active && masterJobs[0]?.status === "failed" ? masterJobs[0] : null;

  const start = () => {
    setError(null);
    synthesizeMaster(classId).catch((e) => setError(String(e)));
  };
  const resume = (jobId: number) => {
    setError(null);
    resumeMasterGuide(jobId).catch((e) => setError(String(e)));
  };

  return (
    <section aria-label="Semester master" className="mt-8 rounded-xl border px-5 py-4">
      <div className="flex min-h-7 items-center justify-between gap-4">
        <h2 className="shrink-0 font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          SEMESTER MASTER
        </h2>

        {active ? (
          <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
            {active.status === "running"
              ? `ELAPSED ${formatElapsed(active.startedAt ?? nowSec, nowSec)}`
              : "QUEUED"}
          </span>
        ) : (
          <span className="flex min-w-0 items-center gap-1.5">
            {guide && (
              <span className="min-w-0 truncate font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
                GENERATED {formatGeneratedAt(guide.generatedAt)}
              </span>
            )}
            {failed ? (
              <>
                {failed.sessionId && (
                  <button
                    type="button"
                    title="Continue the failed run from its claude session"
                    onClick={() => resume(failed.id)}
                    className={`${monoAction} bg-(--accent)/12 text-(--accent) hover:bg-(--accent)/20`}
                  >
                    RESUME
                  </button>
                )}
                <button
                  type="button"
                  onClick={start}
                  className={`${monoAction} text-muted-foreground hover:bg-muted hover:text-foreground`}
                >
                  START OVER
                </button>
              </>
            ) : guide ? (
              guide.stale ? (
                <button
                  type="button"
                  title="Sources changed since this master was generated"
                  onClick={start}
                  className={`${monoAction} bg-class-amber/12 text-class-amber hover:bg-class-amber/20`}
                >
                  STALE — REGENERATE
                </button>
              ) : (
                <button
                  type="button"
                  title="Regenerate the semester master"
                  aria-label="Regenerate the semester master"
                  onClick={start}
                  className="shrink-0 cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
                >
                  <RefreshCw size={12} aria-hidden />
                </button>
              )
            ) : (
              <button
                type="button"
                onClick={start}
                className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
              >
                GENERATE SEMESTER MASTER
              </button>
            )}
            {guide && (
              <button
                type="button"
                onClick={onView}
                className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
              >
                VIEW GUIDE
              </button>
            )}
          </span>
        )}
      </div>

      {active ? (
        <div className="mt-2.5 flex items-center justify-between gap-4">
          <ActiveDetail
            running={active.status === "running"}
            events={output.get(active.id) ?? []}
          />
          {active.status === "running" && (
            <PhaseRail current={derivePhase(output.get(active.id) ?? []).index} />
          )}
        </div>
      ) : failed ? (
        <p className="mt-2 truncate font-mono text-[11px] text-destructive">
          ✕ LAST RUN FAILED — {failed.error ?? "unknown error"}
        </p>
      ) : (
        !guide && (
          <p className="mt-1.5 text-[13px] leading-relaxed text-muted-foreground">
            Reads every module's material fresh and synthesizes cross-module
            connections. Runs alone — expect 30 minutes or more.
          </p>
        )
      )}

      {error && (
        <p className="mt-2 font-mono text-[11px] text-destructive">
          MASTER NOT STARTED — {error}
        </p>
      )}
    </section>
  );
}

function ActiveDetail({
  running,
  events,
}: {
  running: boolean;
  events: readonly ProgressEvent[];
}) {
  const detail = running
    ? derivePhase(events).detail
    : "WAITING FOR RUNNING JOBS TO FINISH — RUNS ALONE";
  return (
    <span className="flex min-w-0 items-center gap-2 font-mono text-[11px] tracking-[0.14em] text-(--accent)">
      <span
        aria-hidden
        className="size-1.5 shrink-0 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
      />
      <span className="min-w-0 truncate">{detail}</span>
    </span>
  );
}
