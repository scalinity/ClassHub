import { useRef, useState, type CSSProperties } from "react";
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
  jobKindLabel,
  rerunAuthCheck,
  toggleJobPanel,
  useJobs,
  type JobInfo,
  type ProgressEvent,
} from "@/lib/jobs";

const ACCENTS: Record<string, string> = {
  blue: "var(--class-blue)",
  orange: "var(--class-orange)",
  green: "var(--class-green)",
  amber: "var(--class-amber)",
};

function accentStyle(color: string | null): CSSProperties {
  return {
    "--accent": (color && ACCENTS[color]) || "var(--muted-foreground)",
  } as CSSProperties;
}

const isActive = (j: JobInfo) => j.status === "running" || j.status === "queued";

export function JobCenter() {
  const { jobs, output, panelOpen, nowSec } = useJobs();
  const [selectedId, setSelectedId] = useState<number | null>(null);

  const running = jobs.filter((j) => j.status === "running");
  const active = jobs.filter(isActive);
  const selected =
    jobs.find((j) => j.id === selectedId) ?? active[0] ?? jobs[0] ?? null;

  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-4 z-40 flex flex-col items-center gap-2 px-4">
      {panelOpen && (
        <section
          aria-label="Job Center"
          className="pointer-events-auto flex w-full max-w-2xl flex-col overflow-hidden rounded-xl border bg-card shadow-xl animate-in fade-in slide-in-from-bottom-2 duration-200 motion-reduce:animate-none"
        >
          <header className="flex items-center justify-between border-b px-4 py-2.5">
            <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
              JOB CENTER
            </h2>
            <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
              {active.length > 0
                ? `${active.length} ACTIVE · MAX 2 CONCURRENT`
                : "NO ACTIVE JOBS"}
            </span>
          </header>

          {jobs.length === 0 ? (
            <p className="px-4 py-8 text-center text-[13px] text-muted-foreground">
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
      />
    </div>
  );
}

function Pill({
  running,
  lastJob,
  nowSec,
  panelOpen,
}: {
  running: JobInfo[];
  lastJob: JobInfo | null;
  nowSec: number;
  panelOpen: boolean;
}) {
  const oldest = running[running.length - 1];
  const failedIdle =
    running.length === 0 && lastJob !== null && lastJob.status === "failed";

  return (
    <button
      type="button"
      aria-expanded={panelOpen}
      aria-label="Toggle Job Center"
      onClick={toggleJobPanel}
      style={accentStyle(oldest?.classColor ?? null)}
      className="pointer-events-auto flex h-9 cursor-pointer items-center gap-2 rounded-full border bg-card px-4 font-mono text-[11px] tracking-[0.14em] shadow-md transition-shadow hover:shadow-lg focus-visible:outline-2 focus-visible:outline-ring"
    >
      {oldest ? (
        <>
          <span
            aria-hidden
            className="size-2 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
          />
          <span className="font-medium">
            {running.length === 1
              ? jobKindLabel(oldest.kind)
              : `${running.length} RUNNING`}
          </span>
          <span className="text-muted-foreground">
            {formatElapsed(oldest.startedAt ?? nowSec, nowSec)}
          </span>
        </>
      ) : failedIdle ? (
        <>
          <X size={11} aria-hidden className="text-destructive" />
          <span className="text-destructive">JOB FAILED</span>
        </>
      ) : (
        <span className="text-muted-foreground">JOBS · IDLE</span>
      )}
      <ChevronUp
        size={12}
        aria-hidden
        className={
          "text-muted-foreground/60 transition-transform duration-150 " +
          (panelOpen ? "rotate-180" : "")
        }
      />
    </button>
  );
}

function StatusIcon({ job }: { job: JobInfo }) {
  switch (job.status) {
    case "running":
      return (
        <span
          aria-hidden
          className="size-2 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
        />
      );
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
}: {
  job: JobInfo;
  nowSec: number;
  selected: boolean;
  onSelect: () => void;
}) {
  const timeLabel =
    job.status === "running"
      ? formatElapsed(job.startedAt ?? nowSec, nowSec)
      : job.status === "queued"
        ? "QUEUED"
        : job.finishedAt
          ? new Date(job.finishedAt * 1000)
              .toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })
              .toUpperCase()
          : "—";

  return (
    <li style={accentStyle(job.classColor)} className="px-1.5">
      <div
        className={
          "flex h-9 items-center gap-2.5 rounded-md px-2.5 " +
          (selected ? "bg-muted/70" : "hover:bg-muted/40")
        }
      >
        <button
          type="button"
          onClick={onSelect}
          aria-label={`Show output of job ${job.id}`}
          className="flex min-w-0 flex-1 cursor-pointer items-center gap-2.5 text-left focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <span className="flex w-4 shrink-0 justify-center">
            <StatusIcon job={job} />
          </span>
          <span className="shrink-0 font-mono text-[11px] font-medium tracking-[0.1em]">
            {jobKindLabel(job.kind)}
          </span>
          <span className="min-w-0 truncate text-[13px] text-muted-foreground">
            {/* scope 'master' is redundant with the MASTER GUIDE kind label */}
            {[job.className, job.scope === "master" ? null : job.scope]
              .filter(Boolean)
              .join(" · ")}
          </span>
        </button>
        <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
          {timeLabel}
        </span>
        {isActive(job) && (
          <button
            type="button"
            title="Cancel job"
            aria-label={`Cancel job ${job.id}`}
            onClick={() => void cancelJob(job.id)}
            className="shrink-0 cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-destructive focus-visible:outline-2 focus-visible:outline-(--accent)"
          >
            <X size={13} aria-hidden />
          </button>
        )}
      </div>
    </li>
  );
}

/** Glyph prefixes encode event kinds: · status, » tool call, ← result, ✓/✕ outcome. */
function OutputLine({ event }: { event: ProgressEvent }) {
  switch (event.kind) {
    case "status":
      return <div className="text-muted-foreground/70">· {event.text}</div>;
    case "tool":
      return (
        <div className="text-muted-foreground">
          <span className="text-(--accent)">»</span> {event.text}
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
          <span className="text-(--accent)">◆</span> {event.text}
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
  events: readonly ProgressEvent[];
}) {
  // Stick to the bottom unless the user scrolls up; the ref callback runs on
  // every render, so new lines keep the pane pinned without effects.
  const stickRef = useRef(true);

  return (
    <div style={accentStyle(job.classColor)} className="border-t">
      <div
        onScroll={(e) => {
          const el = e.currentTarget;
          stickRef.current =
            el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
        ref={(el) => {
          if (el && stickRef.current) el.scrollTop = el.scrollHeight;
        }}
        className="h-52 space-y-1 overflow-y-auto bg-muted/30 px-4 py-3 font-mono text-[11px] leading-relaxed"
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

/**
 * SPEC §6: blocking warning when the spawned claude is not on the subscription.
 * Everything behind it is inert until the check passes.
 */
export function AuthWarning() {
  const { auth } = useJobs();
  if (auth.status !== "failed") return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/60 p-6 backdrop-blur-sm">
      <div
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="auth-warning-title"
        className="w-full max-w-md rounded-xl border bg-card p-6 shadow-2xl"
      >
        <p className="flex items-center gap-2 font-mono text-[11px] tracking-[0.18em] text-destructive">
          <CircleAlert size={13} aria-hidden />
          SUBSCRIPTION AUTH CHECK FAILED
        </p>
        <h2 id="auth-warning-title" className="mt-3 text-[17px] font-semibold">
          Jobs are not using the Max subscription
        </h2>
        <p className="mt-2 text-[13px] leading-relaxed text-muted-foreground">
          {auth.detail}
        </p>
        <p className="mt-2 text-[13px] leading-relaxed text-muted-foreground">
          Running jobs in this state would silently bill pay-per-token API
          credits. Fix the claude CLI login, then re-run the check.
        </p>
        <button
          type="button"
          onClick={() => void rerunAuthCheck()}
          className="mt-5 cursor-pointer rounded-md border px-3.5 py-1.5 text-[12px] font-medium transition-colors hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring"
        >
          Re-run check
        </button>
      </div>
    </div>
  );
}
