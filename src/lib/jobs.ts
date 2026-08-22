import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

export type JobStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

export interface JobInfo {
  id: number;
  kind: string; // probe | self_check | extract | module_guide | ...
  classId: number | null;
  className: string | null;
  classColor: string | null;
  scope: string | null;
  status: JobStatus;
  createdAt: number;
  startedAt: number | null;
  finishedAt: number | null;
  error: string | null;
  summary: string | null;
  logPath: string | null;
  sessionId: string | null;
}

export interface ProgressEvent {
  seq: number;
  kind: "status" | "text" | "tool" | "tool_result" | "retry" | "result" | "error";
  text: string;
}

export interface AuthCheck {
  status: "pending" | "ok" | "failed";
  detail: string;
}

export interface JobsSnapshot {
  jobs: JobInfo[];
  auth: AuthCheck;
  output: ReadonlyMap<number, readonly ProgressEvent[]>;
  panelOpen: boolean;
  /** Unix seconds, ticked every second while a job is active (for elapsed labels). */
  nowSec: number;
}

// --- External store (push-based Tauri events; no useEffect per workspace rules) ---

let snapshot: JobsSnapshot = {
  jobs: [],
  auth: { status: "pending", detail: "" },
  output: new Map(),
  panelOpen: false,
  nowSec: Math.floor(Date.now() / 1000),
};

const storeListeners = new Set<() => void>();
/** Per job: seq -> event. Merges the live stream with the backfill snapshot. */
const buffers = new Map<number, Map<number, ProgressEvent>>();
const subscribedJobs = new Set<number>();
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

function addEvents(jobId: number, events: ProgressEvent[]) {
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
  void listen<ProgressEvent>(`job://${jobId}/progress`, (e) =>
    addEvents(jobId, [e.payload]),
  ).then(() =>
    invoke<ProgressEvent[]>("get_job_events", { jobId }).then((events) =>
      addEvents(jobId, events),
    ),
  );
}

function updateTicker(jobs: JobInfo[]) {
  const active = jobs.some(
    (j) => j.status === "running" || j.status === "queued",
  );
  if (active && ticker === null) {
    ticker = setInterval(
      () => emitChange({ nowSec: Math.floor(Date.now() / 1000) }),
      1000,
    );
  } else if (!active && ticker !== null) {
    clearInterval(ticker);
    ticker = null;
  }
}

async function refreshJobs() {
  const jobs = await invoke<JobInfo[]>("list_jobs");
  emitChange({ jobs, nowSec: Math.floor(Date.now() / 1000) });
  for (const job of jobs) {
    if (job.status === "running" || job.status === "queued") {
      ensureOutput(job.id);
    }
  }
  updateTicker(jobs);
}

let initialized = false;
function init() {
  if (initialized) return;
  initialized = true;
  void listen("jobs-changed", () => void refreshJobs());
  void listen<AuthCheck>("auth-check", (e) => emitChange({ auth: e.payload }));
  void refreshJobs();
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

export function openJobPanel() {
  emitChange({ panelOpen: true });
}

export function toggleJobPanel() {
  emitChange({ panelOpen: !snapshot.panelOpen });
}

/** M3 throwaway: "list the files in this class folder" via claude -p. */
export function runTestJob(classId: number): Promise<number> {
  return invoke<number>("run_test_job", { classId });
}

export function cancelJob(jobId: number): Promise<void> {
  return invoke("cancel_job", { jobId });
}

export function rerunAuthCheck(): Promise<number> {
  return invoke<number>("run_auth_check");
}

// --- Formatting helpers ---

export function formatElapsed(startSec: number, nowSec: number): string {
  const total = Math.max(0, nowSec - startSec);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

export function jobKindLabel(kind: string): string {
  return kind.replace(/_/g, " ").toUpperCase();
}
