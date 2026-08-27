import { invoke } from "@tauri-apps/api/core";

export interface AppSettings {
  aibhsRoot: string;
  /** Whether the configured root exists on disk right now. */
  aibhsRootPresent: boolean;
  jobModel: string;
  jobEffort: string;
  jobConcurrency: number;
  /** Interpreter the on-device transcriber runs through. It lives inside
   *  LocalFlow's bundle, so an update there can move it out from under us. */
  parakeetPython: string;
  parakeetPresent: boolean;
  /** The values the backend's setters accept — the UI renders these. */
  jobModels: string[];
  jobEfforts: string[];
  maxConcurrency: number;
}

/**
 * Display copy for the backend-served option ids. The backend owns which ids
 * exist; an id without copy here still renders (bare, uppercased).
 */
export const JOB_MODEL_COPY: Record<string, { label: string; note: string }> = {
  opus: {
    label: "OPUS",
    note: "The deepest model — the shipped default for guides and extraction.",
  },
  sonnet: {
    label: "SONNET",
    note: "Mid-tier. Faster and lighter on the subscription window.",
  },
  haiku: {
    label: "HAIKU",
    note: "Fastest and cheapest. Fine for throwaway probes, thin for synthesis.",
  },
};

export const JOB_EFFORT_COPY: Record<string, { label: string; note: string }> = {
  low: { label: "LOW", note: "Fewest tokens, fastest turnaround." },
  medium: { label: "MEDIUM", note: "Balanced reading and reasoning." },
  high: { label: "HIGH", note: "The CLI's own default." },
  xhigh: {
    label: "XHIGH",
    note: "Extra-high — the shipped default for study-guide quality.",
  },
  max: { label: "MAX", note: "No ceiling on reasoning. Slowest." },
};

export function optionCopy(
  copy: Record<string, { label: string; note: string }>,
  id: string,
): { label: string; note: string } {
  return copy[id] ?? { label: id.toUpperCase(), note: "" };
}

export function getAppSettings(): Promise<AppSettings> {
  return invoke<AppSettings>("get_app_settings");
}

export function setAibhsRoot(path: string): Promise<void> {
  return invoke("set_aibhs_root", { path });
}

/** Repoints on-device transcription; an empty path restores the default. */
export function setParakeetPython(path: string): Promise<void> {
  return invoke("set_parakeet_python", { path });
}

export function setJobModel(model: string): Promise<void> {
  return invoke("set_job_model", { model });
}

export function setJobEffort(effort: string): Promise<void> {
  return invoke("set_job_effort", { effort });
}

export function setJobConcurrency(count: number): Promise<void> {
  return invoke("set_job_concurrency", { count });
}
