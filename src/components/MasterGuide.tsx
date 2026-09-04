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
import {
  buttonChip,
  buttonFilled,
  buttonIcon,
  buttonText,
  buttonTextMuted,
  errorLine,
  meta,
  pulseDot,
  statusLine,
} from "@/lib/styles";

function PhaseRail({ current }: { current: number }) {
  return (
    <span
      aria-label={`Phase ${current + 1} of ${PHASES.length}: ${PHASES[current]}`}
      className="flex shrink-0 items-center gap-1.5 text-fine"
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
                ? "font-semibold text-(--accent-ink)"
                : i < current
                  ? "text-(--accent-ink)/60"
                  : "text-muted-foreground/50"
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
  practiceActive,
  onView,
  onWatchLive,
}: {
  classId: number;
  guide: GuideInfo | undefined;
  /** A semester-wide practice exam (SPEC §8.3) is queued or running — its
   *  own job, so it neither waits for the master nor blocks it. Derived by
   *  the workspace, the way the other two guide clusters receive it. */
  practiceActive: boolean;
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

  const start = () => {
    setError(null);
    synthesizeMaster(classId).catch((e) =>
      setError(`Not started: ${String(e)}`),
    );
  };
  const resume = (jobId: number) => {
    setError(null);
    resumeMasterGuide(jobId).catch((e) =>
      setError(`Not started: ${String(e)}`),
    );
  };
  const practice = () => {
    setError(null);
    generatePractice(classId, "master").catch((e) =>
      setError(`The practice exam didn't start: ${String(e)}`),
    );
  };

  return (
    <section
      aria-label="Semester master"
      className="mt-8 rounded-xl bg-surface px-5 py-4 ring-1 ring-border"
    >
      <div className="flex min-h-7 flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <h2 className="shrink-0 text-[17px] font-semibold">Semester master</h2>

        {/* The practice action sits outside the master's own state: a
            semester-wide exam neither waits for the master nor blocks it,
            so it is offered — and its pulse shown — during a master run too. */}
        <span className="flex min-w-0 flex-wrap items-center gap-1.5">
          {active ? (
            <span className={meta}>
              {active.status === "running"
                ? `${formatElapsed(active.startedAt ?? nowSec, nowSec)} elapsed`
                : "queued"}
            </span>
          ) : (
            <>
              {guide && (
                <span className={`min-w-0 truncate ${meta}`}>
                  written {formatGeneratedAt(guide.generatedAt)}
                </span>
              )}
              {failed ? (
                <>
                  {failed.sessionId && (
                    <button
                      type="button"
                      title="Continue the failed run from its claude session"
                      onClick={() => resume(failed.id)}
                      className={buttonFilled}
                    >
                      Resume
                    </button>
                  )}
                  <button type="button" onClick={start} className={buttonTextMuted}>
                    Start over
                  </button>
                </>
              ) : guide ? (
                guide.stale ? (
                  <button
                    type="button"
                    title="Sources changed since this master was written"
                    onClick={start}
                    className={`${buttonChip} bg-class-amber/12 text-class-amber hover:bg-class-amber/20`}
                  >
                    Rewrite · sources changed
                  </button>
                ) : (
                  <button
                    type="button"
                    title="Rewrite the semester master"
                    aria-label="Regenerate the semester master"
                    onClick={start}
                    className={buttonIcon}
                  >
                    <RefreshCw size={12} aria-hidden />
                  </button>
                )
              ) : (
                <button type="button" onClick={start} className={buttonText}>
                  Write the semester master
                </button>
              )}
            </>
          )}
          <PracticeAction
            standing
            active={practiceActive}
            onSelect={practice}
          />
          {!active && guide && (
            <button type="button" onClick={onView} className={buttonText}>
              Read guide
            </button>
          )}
        </span>
      </div>

      {active ? (
        <>
          <div className="mt-2.5 flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
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
                  className={`${buttonTextMuted} ${sourceOpen ? "bg-muted text-foreground" : ""}`}
                >
                  Source
                </button>
                <button
                  type="button"
                  title="Watch the document grow as it is written"
                  onClick={() => onWatchLive(active.id)}
                  className={buttonText}
                >
                  Watch live
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
        <p className={`${errorLine} truncate`}>
          The last run failed: {failed.error ?? "unknown error"}
        </p>
      ) : (
        !guide && (
          <p className="mt-1.5 text-body text-muted-foreground">
            Reads every module's material fresh and synthesizes cross-module
            connections. Runs alone — expect 30 minutes or more.
          </p>
        )
      )}

      {error && <p className={errorLine}>{error}</p>}
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
      className="mt-3 h-44 overflow-y-auto rounded-md bg-muted/40 px-3.5 py-2.5 font-mono text-[11px] leading-[1.6] whitespace-pre-wrap break-all text-muted-foreground ring-1 ring-border"
    >
      {text || "Waiting for the writer…"}
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
    : "Waiting for running jobs to finish · runs alone";
  return (
    <span className={`min-w-0 ${statusLine}`}>
      <span aria-hidden className={pulseDot} />
      <span className="min-w-0 truncate">{detail}</span>
    </span>
  );
}
