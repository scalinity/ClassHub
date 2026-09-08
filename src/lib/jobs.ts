import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

import { queryClient } from "@/lib/query";
import { sentence } from "@/lib/utils";

export type JobStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

export interface JobInfo {
  id: number;
  kind: string; // self_check | extract | module_guide | ...
  classId: number | null;
  className: string | null;
  classColor: string | null;
  scope: string | null;
  /** The scope as it is shown: a division's name for a unit scope. */
  scopeLabel: string | null;
  status: JobStatus;
  createdAt: number;
  startedAt: number | null;
  finishedAt: number | null;
  error: string | null;
  summary: string | null;
  logPath: string | null;
  sessionId: string | null;
}

/**
 * Named `Job`-prefixed on purpose: `ProgressEvent` is a DOM global, so a file
 * that uses the bare name without importing it silently resolves to lib.dom
 * instead of failing — the mismatch then surfaces as a baffling structural
 * error rather than "cannot find name".
 */
export interface JobProgressEvent {
  seq: number;
  kind:
    | "status"
    | "text"
    | "tool"
    | "tool_result"
    | "retry"
    | "result"
    | "error"
    | "phase";
  text: string;
  /** Full tool-result text (truncated backend-side); shown behind a disclosure. */
  detail?: string;
}

export interface AuthCheck {
  status: "pending" | "ok" | "failed";
  detail: string;
}

/** One step of the shift's plan (SPEC §6), with what it did. */
export interface ShiftStep {
  name: string;
  state: "pending" | "running" | "done" | "skipped";
  outcome: string | null;
}

export interface ShiftRun {
  id: number;
  /** The date the night's window opened on. */
  night: string;
  trigger: "idle" | "launch" | "manual";
  startedAt: number;
  finishedAt: number | null;
  steps: ShiftStep[];
  jobs: number[];
  stoppedBy: "done" | "budget" | "rate_limit" | "paused" | "window" | "error" | null;
  summary: string | null;
  ownerPid: number | null;
}

/** This week's synthesis in counts and minutes — never dollars (SPEC §12). */
export interface Meter {
  digests: number;
  guides: number;
  exams: number;
  extracts: number;
  minutes: number;
}

export interface ShiftSettings {
  enabled: boolean;
  start: string;
  end: string;
  idleMinutes: number;
  guidesPerNight: number;
  digestsPerNight: number;
  inDevBuild: boolean;
}

export interface ShiftStatus {
  running: boolean;
  /** The run under way, else the last one. */
  run: ShiftRun | null;
  meter: Meter;
  settings: ShiftSettings;
  night: string;
  paused: boolean;
  ranTonight: boolean;
  /** Whether this process runs shifts at all (SPEC §6). */
  runsHere: boolean;
  devBuild: boolean;
}

export interface JobsSnapshot {
  jobs: JobInfo[];
  auth: AuthCheck;
  /** The shift's state, refetched on every `shift-changed` push. */
  shift: ShiftStatus | null;
  output: ReadonlyMap<number, readonly JobProgressEvent[]>;
  /** Rolling decoded source text a synthesis job is writing (live tail). */
  tails: ReadonlyMap<number, string>;
  panelOpen: boolean;
  /** Unix seconds, ticked every second while a job is active (for elapsed labels). */
  nowSec: number;
  /** The gaps the ticker slept through while a job was active — the lid
   *  closed on a running extract — so an elapsed label counts the time a
   *  job actually had rather than the wall clock across the sleep. Cleared
   *  once nothing is active. */
  sleeps: readonly Sleep[];
  /** Set when the job list could not be refreshed, so the panel says so
   *  instead of quietly showing stale rows forever. */
  error: string | null;
}

/** One stretch the one-second ticker missed: when it began, and how long. */
interface Sleep {
  at: number;
  seconds: number;
}

/** A tick a few seconds late is scheduling — the window occluded, the app
 *  napping; one this late is the machine asleep. A job that ran fifty minutes
 *  before the lid closed read as eighty-three on waking, which looks stuck. */
const SLEEP_GAP_SEC = 30;

// --- External store (push-based Tauri events; no useEffect per workspace rules) ---

let snapshot: JobsSnapshot = {
  jobs: [],
  auth: { status: "pending", detail: "" },
  shift: null,
  output: new Map(),
  tails: new Map(),
  panelOpen: false,
  nowSec: Math.floor(Date.now() / 1000),
  sleeps: [],
  error: null,
};

const storeListeners = new Set<() => void>();
/** Per job: seq -> event. Merges the live stream with the backfill snapshot. */
const buffers = new Map<number, Map<number, JobProgressEvent>>();
const subscribedJobs = new Set<number>();
/** Live event registrations, so a settled job can drop its own. */
const unlisteners = new Map<number, () => void>();
const tailUnlisteners = new Map<number, () => void>();
let ticker: ReturnType<typeof setInterval> | null = null;

function emitChange(patch: Partial<JobsSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  for (const notify of storeListeners) notify();
}

function commitOutput(jobId: number) {
  const buffer = buffers.get(jobId);
  const events = buffer
    ? [...buffer.values()].sort((a, b) => a.seq - b.seq)
    : [];
  const output = new Map(snapshot.output);
  output.set(jobId, events);
  emitChange({ output });
}

function addEvents(jobId: number, events: JobProgressEvent[]) {
  let buffer = buffers.get(jobId);
  if (!buffer) {
    buffer = new Map();
    buffers.set(jobId, buffer);
  }
  let changed = false;
  for (const event of events) {
    if (!buffer.has(event.seq)) {
      buffer.set(event.seq, event);
      changed = true;
    }
  }
  if (changed) commitOutput(jobId);
}

/** Attach the live listener first, then backfill; seq-dedupe closes the gap. */
export function ensureOutput(jobId: number) {
  if (subscribedJobs.has(jobId)) return;
  subscribedJobs.add(jobId);
  void listen<JobProgressEvent>(`job://${jobId}/progress`, (e) =>
    addEvents(jobId, [e.payload]),
  )
    .then((unlisten) => {
      // Kept so the registration can be dropped when the job settles —
      // otherwise every job the app has run this session stays subscribed.
      unlisteners.set(jobId, unlisten);
      return invoke<JobProgressEvent[]>("get_job_events", { jobId }).then((events) =>
        addEvents(jobId, events),
      );
    })
    .catch((e) => emitChange({ error: String(e) }));
}

const TAIL_CAP = 8 * 1024;
const subscribedTails = new Set<number>();

function setTail(jobId: number, text: string) {
  const tails = new Map(snapshot.tails);
  tails.set(jobId, text.length > TAIL_CAP ? text.slice(-TAIL_CAP) : text);
  emitChange({ tails });
}

/**
 * Live source feed. Chunks carry no seq, so the backend snapshot (which
 * already contains anything received before it resolves) replaces the buffer;
 * later chunks append.
 */
export function ensureTail(jobId: number) {
  if (subscribedTails.has(jobId)) return;
  subscribedTails.add(jobId);
  let backfilled = false;
  void listen<string>(`job://${jobId}/tail`, (e) => {
    if (!backfilled) return; // covered by the pending backfill snapshot
    setTail(jobId, (snapshot.tails.get(jobId) ?? "") + e.payload);
  })
    .then((unlisten) => {
      tailUnlisteners.set(jobId, unlisten);
      return invoke<string>("get_job_tail", { jobId }).then((tail) => {
        backfilled = true;
        setTail(jobId, tail);
      });
    })
    .catch((e) => emitChange({ error: String(e) }));
}

function updateTicker(jobs: JobInfo[]) {
  const active = jobs.some(
    (j) => j.status === "running" || j.status === "queued",
  );
  if (active && ticker === null) {
    ticker = setInterval(() => {
      const nowSec = Math.floor(Date.now() / 1000);
      const gap = nowSec - snapshot.nowSec;
      const sleeps =
        gap > SLEEP_GAP_SEC
          ? [...snapshot.sleeps, { at: snapshot.nowSec, seconds: gap }]
          : snapshot.sleeps;
      emitChange({ nowSec, sleeps });
    }, 1000);
  } else if (!active && ticker !== null) {
    clearInterval(ticker);
    ticker = null;
    emitChange({ sleeps: [] });
  }
}

async function refreshJobs() {
  let jobs: JobInfo[];
  try {
    jobs = await invoke<JobInfo[]>("list_jobs");
  } catch (e) {
    // Without this the panel silently freezes on stale rows: this runs from an
    // event listener, so a rejection would only ever reach the console.
    emitChange({ error: String(e) });
    return;
  }
  // Detected before the state is updated: the active → settled edge is what
  // says an artifact just landed.
  const settled = jobs.filter((j) => !isActive(j) && wasActive.delete(j.id));
  emitChange({ jobs, nowSec: Math.floor(Date.now() / 1000), error: null });
  for (const job of jobs) {
    if (isActive(job)) {
      wasActive.add(job.id);
      ensureOutput(job.id);
    } else {
      releaseJob(job.id);
    }
  }
  // A job that produces an artifact reconciles it backend-side before its row
  // leaves running, so this transition is the moment to refetch. Invalidating
  // here rather than encoding a job counter into the query key keeps the keys
  // stable, and matches how hub-changed already drives every other refetch.
  for (const job of settled) {
    if (
      job.kind === "module_guide" ||
      job.kind === "master_guide" ||
      job.kind === "lecture_digest"
    ) {
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
      // A finished digest wrote the corpus note behind a contribution, which
      // is what turns "mapped" into "feeding a guide" on both listings.
      if (job.kind === "lecture_digest") {
        void queryClient.invalidateQueries({ queryKey: ["contributions"] });
        void queryClient.invalidateQueries({ queryKey: ["hints"] });
      }
    } else if (job.kind === "practice") {
      void queryClient.invalidateQueries({ queryKey: ["practice"] });
    }
  }
  updateTicker(jobs);
}

const isActive = (j: JobInfo) => j.status === "running" || j.status === "queued";
/** Ids seen active, so the active → settled edge can be detected. */
const wasActive = new Set<number>();

/**
 * Drops a settled job's live registrations. The condensed event list stays —
 * the Job Center still renders it for a finished run — but the listener and
 * the growing source tail have nothing left to feed.
 */
function releaseJob(jobId: number) {
  const unlisten = unlisteners.get(jobId);
  if (unlisten) {
    unlisten();
    unlisteners.delete(jobId);
    subscribedJobs.delete(jobId);
  }
  const tailUnlisten = tailUnlisteners.get(jobId);
  if (tailUnlisten) {
    tailUnlisten();
    tailUnlisteners.delete(jobId);
    subscribedTails.delete(jobId);
  }
}

/** The shift's state, from the run row and the settings (SPEC §6). */
async function refreshShift() {
  try {
    const shift = await invoke<ShiftStatus>("get_shift_status");
    emitChange({ shift, error: null });
  } catch (e) {
    emitChange({ error: String(e) });
  }
}

let initialized = false;
function init() {
  if (initialized) return;
  initialized = true;
  void listen("jobs-changed", () => void refreshJobs());
  void listen("shift-changed", () => void refreshShift());
  void listen<AuthCheck>("auth-check", (e) => emitChange({ auth: e.payload }));
  void refreshJobs();
  void refreshShift();
  void invoke<AuthCheck>("get_auth_check").then((auth) => emitChange({ auth }));
}
init();

export function useJobs(): JobsSnapshot {
  return useSyncExternalStore(
    (cb) => {
      storeListeners.add(cb);
      return () => storeListeners.delete(cb);
    },
    () => snapshot,
  );
}

// --- Actions ---

export function toggleJobPanel() {
  emitChange({ panelOpen: !snapshot.panelOpen });
}

export function cancelJob(jobId: number): Promise<void> {
  return invoke("cancel_job", { jobId });
}

export function rerunAuthCheck(): Promise<number> {
  return invoke<number>("run_auth_check");
}

/** `Run the shift now` (SPEC §6): once a night, like the idle trigger. */
export function runShiftNow(): Promise<number> {
  return invoke<number>("run_shift_now");
}

/** `Pause tonight`, or its undo; a run under way stops between jobs. */
export function pauseShiftTonight(paused: boolean): Promise<void> {
  return invoke("pause_shift_tonight", { paused });
}

/** `2 h 10 m`, `45 m`, `0 m` — the meter's minutes (SPEC §12). */
export function formatMinutes(minutes: number): string {
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return hours > 0 ? `${hours} h ${rest} m` : `${rest} m`;
}

// --- Formatting helpers ---

/** `MM:SS` a job has run — the wall clock since it started, less the sleeps
 *  the store saw begin after that (a sleep the job started after is not
 *  its). Read off the store rather than threaded through as a prop: it only
 *  ever changes together with `nowSec`, which every caller already has. */
export function formatElapsed(startSec: number, nowSec: number): string {
  const slept = snapshot.sleeps
    .filter((s) => s.at >= startSec)
    .reduce((sum, s) => sum + s.seconds, 0);
  const total = Math.max(0, nowSec - startSec - slept);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

/** What a job is called on screen, by its Rust kind. `module_guide` also
 *  builds a guide for a Week or a Part (SPEC §8.1) — the kind kept its
 *  original name, and the reader is never shown "module" for a division the
 *  course calls something else. */
const JOB_KIND_LABELS: Record<string, string> = {
  self_check: "Self-check",
  extract: "Extract",
  module_guide: "Study guide",
  master_guide: "Semester master",
  practice: "Practice exam",
  lecture_digest: "Lecture digest",
  sort_proposal: "Sort",
  syllabus_scan: "Syllabus scan",
};

export function jobKindLabel(kind: string): string {
  const label = JOB_KIND_LABELS[kind];
  return typeof label === "string" ? label : sentence(kind.replace(/_/g, " "));
}

/**
 * SPEC §8.2 long-job UX: the run's real phases, derived from the stream.
 * Orient until the first source read; Read while extracts stream in; Compose
 * once Write input starts streaming (`phase` events carry live byte counts);
 * Verify when the model greps/re-reads after writing.
 */
export const PHASES = ["Orient", "Read", "Compose", "Verify"] as const;

export function derivePhase(events: readonly JobProgressEvent[]): {
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
  if (verifying) return { index: 3, detail: "Verifying the output" };
  if (composing) {
    return {
      index: 2,
      detail: kb === null ? "Composing the guide" : `Composing · ~${kb} KB written`,
    };
  }
  if (reads > 0) {
    return {
      index: 1,
      detail: `Reading sources · ${reads} ${reads === 1 ? "read" : "reads"}`,
    };
  }
  return { index: 0, detail: "Orienting" };
}
