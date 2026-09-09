import { useRef, useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Check,
  ChevronRight,
  ChevronUp,
  CircleAlert,
  Clock,
  Minus,
  X,
} from "lucide-react";

import {
  cancelJob,
  ensureOutput,
  formatElapsed,
  formatMinutes,
  jobKindLabel,
  pauseShiftTonight,
  rerunAuthCheck,
  runShiftNow,
  toggleJobPanel,
  useJobs,
  type JobInfo,
  type JobProgressEvent,
  type ShiftRun,
  type ShiftStatus,
  type ShiftStep,
} from "@/lib/jobs";
import { NoticeLine } from "@/components/NoticeToast";
import { CLASS_ACCENTS } from "@/lib/classes";
import { useNotices } from "@/lib/notices";
import { formatClock, formatStamp, formatTime } from "@/lib/schedule";
import { getAppSettings } from "@/lib/settings";
import {
  buttonIcon,
  buttonTextNeutral,
  chipMuted,
  errorLine,
  meta,
  pulseDot,
} from "@/lib/styles";

function accentStyle(color: string | null): CSSProperties {
  return {
    "--accent": (color && CLASS_ACCENTS[color]) || "var(--muted-foreground)",
  } as CSSProperties;
}

const isActive = (j: JobInfo) => j.status === "running" || j.status === "queued";

export function JobCenter() {
  const { jobs, output, panelOpen, nowSec, error, shift } = useJobs();
  const { notices } = useNotices();
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const notice = actionError ?? error;
  const { data: appSettings } = useQuery({
    queryKey: ["appSettings"],
    queryFn: getAppSettings,
  });

  const running = jobs.filter((j) => j.status === "running");
  const active = jobs.filter(isActive);
  const selected =
    jobs.find((j) => j.id === selectedId) ?? active[0] ?? jobs[0] ?? null;

  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-4 z-40 flex flex-col items-center gap-2 px-4">
      {panelOpen && (
        <section
          aria-label="Job Center"
          className="pointer-events-auto flex w-full max-w-2xl flex-col overflow-hidden rounded-xl bg-surface shadow-xl ring-1 ring-border animate-in fade-in slide-in-from-bottom-2 duration-200 motion-reduce:animate-none"
        >
          <header className="flex items-center justify-between border-b border-border/70 px-4 py-2.5">
            <h2 className="text-[15px] font-semibold">Jobs</h2>
            <span className={meta}>
              {active.length > 0
                ? `${active.length} active · up to ${appSettings?.jobConcurrency ?? 2} at once`
                : "Nothing running"}
            </span>
          </header>

          {shift !== null && <ShiftPanel shift={shift} onError={setActionError} />}

          {notice !== null && (
            <p
              role="alert"
              className="border-b border-border/70 bg-destructive/5 px-4 py-2 text-body text-destructive"
            >
              {notice}
            </p>
          )}

          {notices.length > 0 && (
            // The last few reversible actions, with their Undo, once the
            // toast has faded (SPEC §12).
            <ul aria-label="Recent actions" className="border-b border-border/70 px-4 py-2">
              {notices.map((recent) => (
                <li key={recent.key} className="flex min-h-8 items-center gap-2">
                  <NoticeLine notice={recent} />
                  <span className={`shrink-0 ${meta}`}>
                    {formatClock(new Date(recent.at))}
                  </span>
                </li>
              ))}
            </ul>
          )}

          {jobs.length === 0 ? (
            <p className="px-4 py-8 text-center text-body text-muted-foreground">
              No jobs yet. Extraction and synthesis work will appear here.
            </p>
          ) : (
            <>
              <ul className="max-h-44 overflow-y-auto py-1">
                {jobs.map((job) => (
                  <JobRow
                    key={job.id}
                    job={job}
                    nowSec={nowSec}
                    selected={selected?.id === job.id}
                    onSelect={() => {
                      setSelectedId(job.id);
                      ensureOutput(job.id);
                    }}
                    onError={setActionError}
                  />
                ))}
              </ul>
              {selected && (
                <OutputPane
                  job={selected}
                  events={output.get(selected.id) ?? []}
                />
              )}
            </>
          )}
        </section>
      )}

      <Pill
        running={running}
        lastJob={jobs[0] ?? null}
        nowSec={nowSec}
        panelOpen={panelOpen}
        shift={shift}
      />
    </div>
  );
}

/** `step 3 of 5` while a run is under way. */
function shiftStepLabel(run: ShiftRun): string | null {
  const index = run.steps.findIndex((s) => s.state === "running");
  return index === -1 ? null : `step ${index + 1} of ${run.steps.length}`;
}

function Pill({
  running,
  lastJob,
  nowSec,
  panelOpen,
  shift,
}: {
  running: JobInfo[];
  lastJob: JobInfo | null;
  nowSec: number;
  panelOpen: boolean;
  shift: ShiftStatus | null;
}) {
  const oldest = running[running.length - 1];
  const failedIdle =
    running.length === 0 && lastJob !== null && lastJob.status === "failed";
  const shiftRun = shift?.running ? shift.run : null;

  return (
    <button
      type="button"
      aria-expanded={panelOpen}
      aria-label="Toggle Job Center"
      onClick={toggleJobPanel}
      style={accentStyle(oldest?.classColor ?? null)}
      className="pointer-events-auto flex h-9 cursor-pointer items-center gap-2 rounded-full bg-surface px-4 text-body shadow-md ring-1 ring-border transition-shadow hover:shadow-lg focus-visible:outline-2 focus-visible:outline-ring"
    >
      {shiftRun ? (
        <>
          <span aria-hidden className={pulseDot} />
          <span className="font-medium">
            {`Shift · ${shiftStepLabel(shiftRun) ?? "starting"}`}
          </span>
          {oldest && (
            <span className="tabular-nums text-muted-foreground">
              {jobKindLabel(oldest.kind)} {formatElapsed(oldest.startedAt ?? nowSec, nowSec)}
            </span>
          )}
        </>
      ) : oldest ? (
        <>
          <span aria-hidden className={pulseDot} />
          <span className="font-medium">
            {running.length === 1
              ? `Jobs · ${jobKindLabel(oldest.kind)}`
              : `Jobs · ${running.length} running`}
          </span>
          <span className="tabular-nums text-muted-foreground">
            {formatElapsed(oldest.startedAt ?? nowSec, nowSec)}
          </span>
        </>
      ) : failedIdle ? (
        <>
          <X size={12} aria-hidden className="text-destructive" />
          <span className="text-destructive">Jobs · failed</span>
        </>
      ) : (
        <span className="text-muted-foreground">Jobs · idle</span>
      )}
      <ChevronUp
        size={12}
        aria-hidden
        className={
          "text-muted-foreground/70 transition-transform duration-150 " +
          (panelOpen ? "rotate-180" : "")
        }
      />
    </button>
  );
}

function StatusIcon({ job }: { job: JobInfo }) {
  switch (job.status) {
    case "running":
      return <span aria-hidden className={pulseDot} />;
    case "queued":
      return <Clock size={11} aria-hidden className="text-muted-foreground/70" />;
    case "succeeded":
      return <Check size={12} aria-hidden className="text-muted-foreground" />;
    case "failed":
      return <X size={12} aria-hidden className="text-destructive" />;
    case "cancelled":
      return <Minus size={12} aria-hidden className="text-muted-foreground/60" />;
  }
}

function JobRow({
  job,
  nowSec,
  selected,
  onSelect,
  onError,
}: {
  job: JobInfo;
  nowSec: number;
  selected: boolean;
  onSelect: () => void;
  onError: (message: string | null) => void;
}) {
  const timeLabel =
    job.status === "running"
      ? formatElapsed(job.startedAt ?? nowSec, nowSec)
      : job.status === "queued"
        ? "queued"
        : job.finishedAt
          ? formatClock(new Date(job.finishedAt * 1000))
          : "—";

  return (
    <li style={accentStyle(job.classColor)} className="px-1.5">
      <div
        className={
          "flex min-h-9 items-center gap-2.5 rounded-md px-2.5 " +
          (selected ? "bg-muted/70" : "hover:bg-muted/40")
        }
      >
        <button
          type="button"
          onClick={onSelect}
          aria-label={`Show output of job ${job.id}`}
          className="flex min-w-0 flex-1 cursor-pointer items-center gap-2.5 rounded-sm text-left focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <span className="flex w-4 shrink-0 justify-center">
            <StatusIcon job={job} />
          </span>
          <span className={chipMuted}>{jobKindLabel(job.kind)}</span>
          <span
            title={job.summary ?? undefined}
            className="min-w-0 truncate text-body text-muted-foreground"
          >
            {/* scope 'master' is redundant with the Semester master kind label */}
            {[job.className, job.scope === "master" ? null : job.scopeLabel]
              .filter(Boolean)
              .join(" · ")}
            {/* What a finished job recorded, in its own words. */}
            {job.summary && job.status === "succeeded" && (
              <span className="text-muted-foreground/70"> — {job.summary}</span>
            )}
          </span>
        </button>
        <span className={`shrink-0 ${meta}`}>{timeLabel}</span>
        {isActive(job) && (
          <button
            type="button"
            title="Cancel job"
            aria-label={`Cancel job ${job.id}`}
            onClick={() => {
              onError(null);
              cancelJob(job.id).catch((e) => onError(String(e)));
            }}
            className={`${buttonIcon} hover:text-destructive`}
          >
            <X size={13} aria-hidden />
          </button>
        )}
      </div>
    </li>
  );
}

/** Glyph prefixes encode event kinds: · status, » tool call, ← result, ✓/✕ outcome. */
function OutputLine({ event }: { event: JobProgressEvent }) {
  switch (event.kind) {
    case "status":
      return <div className="text-muted-foreground/70">· {event.text}</div>;
    case "tool":
      return (
        <div className="text-muted-foreground">
          <span className="text-(--accent-ink)">»</span> {event.text}
        </div>
      );
    case "tool_result":
      if (!event.detail) {
        return <div className="text-muted-foreground/60">← {event.text}</div>;
      }
      return (
        <details className="group">
          <summary className="flex cursor-pointer list-none items-center gap-1.5 text-muted-foreground/60 transition-colors hover:text-muted-foreground focus-visible:outline-2 focus-visible:outline-(--accent) [&::-webkit-details-marker]:hidden">
            <span>← {event.text}</span>
            <ChevronRight
              size={10}
              aria-hidden
              className="shrink-0 text-muted-foreground/40 transition-transform duration-150 group-open:rotate-90 motion-reduce:transition-none"
            />
          </summary>
          <pre className="mt-1.5 mb-1 max-h-48 overflow-y-auto whitespace-pre-wrap border-l pl-3 font-mono text-muted-foreground/80">
            {event.detail}
          </pre>
        </details>
      );
    case "phase":
      return (
        <div className="text-muted-foreground">
          <span className="text-(--accent-ink)">◆</span> {event.text}
        </div>
      );
    case "retry":
      return <div className="italic text-muted-foreground">↻ {event.text}</div>;
    case "error":
      return <div className="text-destructive">✕ {event.text}</div>;
    case "result":
      return <div className="font-medium">✓ {event.text}</div>;
    default:
      return (
        <div className="whitespace-pre-wrap text-foreground/90">{event.text}</div>
      );
  }
}

function OutputPane({
  job,
  events,
}: {
  job: JobInfo;
  events: readonly JobProgressEvent[];
}) {
  // Stick to the bottom unless the user scrolls up; the ref callback runs on
  // every render, so new lines keep the pane pinned without effects.
  const stickRef = useRef(true);

  return (
    <div style={accentStyle(job.classColor)} className="border-t border-border/70">
      <div
        onScroll={(e) => {
          const el = e.currentTarget;
          stickRef.current =
            el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
        ref={(el) => {
          if (el && stickRef.current) el.scrollTop = el.scrollHeight;
        }}
        className="h-52 space-y-1 overflow-y-auto bg-muted/40 px-4 py-3 font-mono text-[12px] leading-relaxed"
      >
        {events.length === 0 ? (
          <p className="text-muted-foreground/70">
            {isActive(job)
              ? "· waiting for output…"
              : job.logPath
                ? `no captured output — raw log: ${job.logPath}`
                : "no output captured"}
          </p>
        ) : (
          events.map((event) => <OutputLine key={event.seq} event={event} />)
        )}
      </div>
    </div>
  );
}

/** What the run's title line says about how it ended. */
const STOPPED_BY_COPY: Record<NonNullable<ShiftRun["stoppedBy"]>, string> = {
  done: "",
  budget: "· capped for the night",
  rate_limit: "· stopped at the rate limit",
  paused: "· paused",
  window: "· ended with its window",
  error: "· stopped",
};

function runTitle(run: ShiftRun, running: boolean): string {
  const started = new Date(run.startedAt * 1000);
  if (running) {
    const by = run.trigger === "manual" ? "started by hand" : run.trigger === "launch" ? "catching up" : "running";
    return `Tonight's shift · ${by} since ${formatClock(started)}`;
  }
  const ended = run.finishedAt ? formatStamp(new Date(run.finishedAt * 1000)) : formatStamp(started);
  const how = run.stoppedBy ? STOPPED_BY_COPY[run.stoppedBy] : "";
  return `Last shift · ${ended} ${how}`.trim();
}

/** Tonight, in one line: when it runs, or why it will not. */
function tonightLine(shift: ShiftStatus): string {
  const { settings, running, ranTonight, paused, runsHere } = shift;
  if (running) return "Running now";
  if (!runsHere) return "Runs in the installed app, not this build";
  if (!settings.enabled) return "Off — switch it on in Settings";
  if (paused) return "Paused tonight";
  if (ranTonight) return "Ran tonight · next window tomorrow";
  const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
  return `Tonight from ${formatTime(settings.start)} once the Mac has been idle ${settings.idleMinutes} min · up to ${count(settings.digestsPerNight, "digest", "digests")}, ${count(settings.guidesPerNight, "guide", "guides")}, ${count(settings.briefsPerNight, "brief", "briefs")}`;
}

function StepMark({ state }: { state: ShiftStep["state"] }) {
  switch (state) {
    case "running":
      return <span aria-hidden className={pulseDot} />;
    case "done":
      return <Check size={12} aria-hidden className="text-muted-foreground" />;
    case "skipped":
      return <Minus size={12} aria-hidden className="text-muted-foreground/60" />;
    default:
      return (
        <span
          aria-hidden
          className="size-1.5 shrink-0 rounded-full border border-muted-foreground/50"
        />
      );
  }
}

/**
 * SPEC §6 — the shift's place in the panel: this week's meter in counts and
 * minutes, the current or last run's plan with each step's outcome, tonight
 * in one line, and the two controls the tray also carries.
 */
function ShiftPanel({
  shift,
  onError,
}: {
  shift: ShiftStatus;
  onError: (message: string | null) => void;
}) {
  const { run, meter, running, paused, ranTonight } = shift;
  const [busy, setBusy] = useState(false);
  const act = (change: Promise<unknown>) => {
    setBusy(true);
    onError(null);
    change.catch((e) => onError(String(e))).finally(() => setBusy(false));
  };

  return (
    <section
      aria-label="The shift"
      style={accentStyle(null)}
      className="border-b border-border/70 px-4 py-3"
    >
      <div className="flex items-baseline justify-between gap-3">
        <p className="min-w-0 truncate text-body font-medium">
          {run ? runTitle(run, running) : "The shift has not run yet"}
        </p>
        <p className={`shrink-0 ${meta}`}>
          This week · {meter.digests} {meter.digests === 1 ? "digest" : "digests"} ·{" "}
          {meter.guides} {meter.guides === 1 ? "guide" : "guides"}
          {meter.documents > 0 &&
            ` · ${meter.documents} ${meter.documents === 1 ? "document" : "documents"}`}
          {" · "}
          {formatMinutes(meter.minutes)}
        </p>
      </div>
      {run?.summary && (
        <p className="mt-0.5 text-body text-muted-foreground">{run.summary}</p>
      )}
      {run && (
        <ol aria-label="The plan" className="mt-2 space-y-1">
          {run.steps.map((step, index) => (
            <li key={step.name} className="flex min-h-5 items-center gap-2.5 text-body">
              <span className="flex w-4 shrink-0 justify-center">
                <StepMark state={step.state} />
              </span>
              <span className={`w-4 shrink-0 ${meta}`}>{index + 1}</span>
              <span className={step.state === "pending" ? "text-muted-foreground" : ""}>
                {step.name}
              </span>
              {step.outcome && (
                <span className="min-w-0 truncate text-muted-foreground" title={step.outcome}>
                  · {step.outcome}
                </span>
              )}
            </li>
          ))}
        </ol>
      )}
      <div className="mt-2.5 flex items-center gap-1">
        <span className={`min-w-0 flex-1 truncate ${meta}`}>{tonightLine(shift)}</span>
        <button
          type="button"
          disabled={busy || running || ranTonight}
          onClick={() => act(runShiftNow())}
          className={buttonTextNeutral}
        >
          Run the shift now
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => act(pauseShiftTonight(!paused))}
          className={buttonTextNeutral}
        >
          {paused ? "Resume tonight" : "Pause tonight"}
        </button>
      </div>
    </section>
  );
}

/**
 * SPEC §6: blocking warning when the spawned claude is not on the subscription.
 * Everything behind it is inert until the check passes.
 */
export function AuthWarning() {
  const { auth } = useJobs();
  const [recheckError, setRecheckError] = useState<string | null>(null);
  if (auth.status !== "failed") return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/60 p-6 backdrop-blur-sm">
      <div
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="auth-warning-title"
        className="w-full max-w-md rounded-xl bg-surface p-6 shadow-2xl ring-1 ring-border"
      >
        <p className="flex items-center gap-2 text-body font-medium text-destructive">
          <CircleAlert size={13} aria-hidden />
          Subscription check failed
        </p>
        <h2 id="auth-warning-title" className="mt-3 text-headline">
          Jobs are not using the Max subscription
        </h2>
        <p className="mt-2 text-body text-muted-foreground">{auth.detail}</p>
        <p className="mt-2 text-body text-muted-foreground">
          Running jobs in this state would silently bill pay-per-token API
          credits. Fix the claude CLI login, then re-run the check.
        </p>
        <button
          type="button"
          onClick={() => {
            setRecheckError(null);
            rerunAuthCheck().catch((e) => setRecheckError(String(e)));
          }}
          className={`${buttonTextNeutral} mt-5 ring-1 ring-border`}
        >
          Re-run check
        </button>
        {recheckError !== null && <p className={errorLine}>{recheckError}</p>}
      </div>
    </div>
  );
}
