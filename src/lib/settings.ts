import { invoke } from "@tauri-apps/api/core";

export interface AppSettings {
  aibhsRoot: string;
  /** Whether the configured root exists on disk right now. */
  aibhsRootPresent: boolean;
  jobModel: string;
  jobEffort: string;
  jobConcurrency: number;
}

/** CLI model aliases — each resolves to its current release at spawn time. */
export const JOB_MODELS = [
  {
    id: "opus",
    label: "OPUS",
    note: "The deepest model — the shipped default for guides and extraction.",
  },
  {
    id: "sonnet",
    label: "SONNET",
    note: "Mid-tier. Faster and lighter on the subscription window.",
  },
  {
    id: "haiku",
    label: "HAIKU",
    note: "Fastest and cheapest. Fine for throwaway probes, thin for synthesis.",
  },
] as const;

/** The CLI's --effort ladder, cheapest first. */
export const JOB_EFFORTS = [
  { id: "low", label: "LOW", note: "Fewest tokens, fastest turnaround." },
  { id: "medium", label: "MEDIUM", note: "Balanced reading and reasoning." },
  { id: "high", label: "HIGH", note: "The CLI's own default." },
  {
    id: "xhigh",
    label: "XHIGH",
    note: "Extra-high — the shipped default for study-guide quality.",
  },
  { id: "max", label: "MAX", note: "No ceiling on reasoning. Slowest." },
] as const;

export const MAX_JOB_CONCURRENCY = 4;

export function getAppSettings(): Promise<AppSettings> {
  return invoke<AppSettings>("get_app_settings");
}

export function setAibhsRoot(path: string): Promise<void> {
  return invoke("set_aibhs_root", { path });
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
