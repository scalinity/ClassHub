import { invoke } from "@tauri-apps/api/core";

import type { ShiftSettings } from "@/lib/jobs";

/** One job kind's own model and effort (SPEC §6); null is the global pair. */
export interface JobKindSettings {
  kind: string;
  label: string;
  model: string | null;
  effort: string | null;
}

export interface AppSettings {
  aibhsRoot: string;
  /** Whether the configured root exists on disk right now. */
  aibhsRootPresent: boolean;
  jobModel: string;
  jobEffort: string;
  jobConcurrency: number;
  jobKinds: JobKindSettings[];
  /** Interpreter the on-device transcriber runs through. It lives inside
   *  LocalFlow's bundle, so an update there can move it out from under us. */
  parakeetPython: string;
  parakeetPresent: boolean;
  /** The values the backend's setters accept — the UI renders these. */
  jobModels: string[];
  jobEfforts: string[];
  maxConcurrency: number;
  shift: ShiftSettings;
  notifyShiftFinished: boolean;
  notifyJobFailed: boolean;
  notifyDueTomorrow: boolean;
  notifyAnnouncementAction: boolean;
  /** `HH:MM`, when the due-tomorrow notification is shown. */
  notifyDueTime: string;
  /** Whether the app is registered as a login item (SPEC §12). */
  loginItem: boolean;
  /** A dev build, where the shift's "run in this build" switch means something. */
  devBuild: boolean;
}

/**
 * Display copy for the backend-served option ids. The backend owns which ids
 * exist; an id without copy here still renders, bare.
 */
export const JOB_MODEL_COPY: Record<string, { label: string; note: string }> = {
  opus: {
    label: "Opus",
    note: "The deepest model — the shipped default for guides and extraction.",
  },
  sonnet: {
    label: "Sonnet",
    note: "Mid-tier. Faster and lighter on the subscription window.",
  },
  haiku: {
    label: "Haiku",
    note: "Fastest and cheapest. Fine for throwaway probes, thin for synthesis.",
  },
};

export const JOB_EFFORT_COPY: Record<string, { label: string; note: string }> = {
  low: { label: "Low", note: "Fewest tokens, fastest turnaround." },
  medium: { label: "Medium", note: "Balanced reading and reasoning." },
  high: { label: "High", note: "The CLI's own default." },
  xhigh: {
    label: "Extra high",
    note: "The shipped default for study-guide quality.",
  },
  max: { label: "Max", note: "No ceiling on reasoning. Slowest." },
};

export function optionCopy(
  copy: Record<string, { label: string; note: string }>,
  id: string,
): { label: string; note: string } {
  return copy[id] ?? { label: id, note: "" };
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

/** A kind's own model (SPEC §6); null returns it to the global pair. */
export function setJobKindModel(kind: string, model: string | null): Promise<void> {
  return invoke("set_job_kind_model", { kind, model });
}

export function setJobKindEffort(kind: string, effort: string | null): Promise<void> {
  return invoke("set_job_kind_effort", { kind, effort });
}

/** The shift's settings by key, as the backend names them (SPEC §6). */
export type ShiftSettingKey =
  | "shift_enabled"
  | "shift_start"
  | "shift_end"
  | "shift_idle_minutes"
  | "shift_guides_per_night"
  | "shift_digests_per_night"
  | "shift_briefs_per_night"
  | "shift_in_dev_build";

export function setShiftSetting(key: ShiftSettingKey, value: string): Promise<void> {
  return invoke("set_shift_setting", { key, value });
}

export type NotifyKey =
  | "notify_shift_finished"
  | "notify_job_failed"
  | "notify_due_tomorrow"
  | "notify_announcement_action";

export function setNotifySetting(key: NotifyKey, on: boolean): Promise<void> {
  return invoke("set_notify_setting", { key, on });
}

/** When the due-tomorrow notification is shown, `HH:MM`. */
export function setNotifyDueTime(time: string): Promise<void> {
  return invoke("set_notify_due_time", { time });
}

/** Registers or removes the login item (SPEC §12). */
export function setLoginItem(on: boolean): Promise<void> {
  return invoke("set_login_item", { on });
}
