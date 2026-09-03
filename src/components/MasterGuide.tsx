import { Fragment, useState } from "react";
import { RefreshCw } from "lucide-react";

import { PracticeAction } from "@/components/PracticeAction";
import {
  formatGeneratedAt,
  generatePractice,
  resumeMasterGuide,
  synthesizeMaster,
  type GuideInfo,
} from "@/lib/guides";
import {
  derivePhase,
  ensureTail,
  formatElapsed,
  PHASES,
  useJobs,
  type JobProgressEvent,
} from "@/lib/jobs";
import { monoAction } from "@/lib/styles";

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
  onWatchLive,
}: {
  classId: number;
  guide: GuideInfo | undefined;
  onView: () => void;
  /** Open the live document preview of the file the given job is composing. */
  onWatchLive: (jobId: number) => void;
}) {
  const { jobs, output, tails, nowSec } = useJobs();
  const [error, setError] = useState<string | null>(null);
  const [sourceOpen, setSourceOpen] = useState(false);

  const masterJobs = jobs.filter(
    (j) => j.kind === "master_guide" && j.classId === classId,
  );
  const active =
    masterJobs.find((j) => j.status === "running" || j.status === "queued") ??
    null;
  const failed =
    !active && masterJobs[0]?.status === "failed" ? masterJobs[0] : null;
  // A semester-wide practice exam (SPEC §8.3) is its own job, so it neither
  // waits for the master nor blocks it.
  const practiceActive = jobs.some(
    (j) =>
      j.kind === "practice" &&
      j.classId === classId &&
      j.scope === "master" &&
      (j.status === "running" || j.status === "queued"),
  );

  const start = () => {
    setError(null);
    synthesizeMaster(classId).catch((e) =>
      setError(`MASTER NOT STARTED — ${String(e)}`),
    );
  };
  const resume = (jobId: number) => {
    setError(null);
    resumeMasterGuide(jobId).catch((e) =>
      setError(`MASTER NOT STARTED — ${String(e)}`),
    );
  };
  const practice = () => {
    setError(null);
    generatePractice(classId, "master").catch((e) =>
      setError(`PRACTICE EXAM NOT STARTED — ${String(e)}`),
    );
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
            <PracticeAction standing active={practiceActive} onSelect={practice} />
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
        <>
          <div className="mt-2.5 flex items-center justify-between gap-4">
            <ActiveDetail
              running={active.status === "running"}
              events={output.get(active.id) ?? []}
            />
            {active.status === "running" && (
              <span className="flex shrink-0 items-center gap-3">
                <button
                  type="button"
                  title="Show the source text as the model writes it"
                  aria-expanded={sourceOpen}
                  onClick={() => {
                    ensureTail(active.id);
                    setSourceOpen((open) => !open);
                  }}
                  className={`${monoAction} ${sourceOpen ? "bg-(--accent)/12 text-(--accent)" : "text-muted-foreground hover:bg-muted hover:text-foreground"}`}
                >
                  SOURCE
                </button>
                <button
                  type="button"
                  title="Watch the document grow as it is written"
                  onClick={() => onWatchLive(active.id)}
                  className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
                >
                  WATCH LIVE
                </button>
                <PhaseRail
                  current={derivePhase(output.get(active.id) ?? []).index}
                />
              </span>
            )}
          </div>
          {sourceOpen && active.status === "running" && (
            <SourceFeed text={tails.get(active.id) ?? ""} />
          )}
        </>
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
        <p className="mt-2 font-mono text-[11px] text-destructive">{error}</p>
      )}
    </section>
  );
}

/**
 * The raw source ticker: the last few KB of the file as the model streams it,
 * pinned to the bottom like tail -f. The inline ref runs on every commit, so
 * new chunks keep the feed stuck to the newest line.
 */
function SourceFeed({ text }: { text: string }) {
  return (
    <pre
      aria-label="Live source feed"
      ref={(el) => {
        if (el) el.scrollTop = el.scrollHeight;
      }}
      className="mt-3 h-44 overflow-y-auto rounded-lg border bg-muted/30 px-3.5 py-2.5 font-mono text-[10px] leading-[1.6] whitespace-pre-wrap break-all text-muted-foreground"
    >
      {text || "WAITING FOR THE WRITER…"}
    </pre>
  );
}

function ActiveDetail({
  running,
  events,
}: {
  running: boolean;
  events: readonly JobProgressEvent[];
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
