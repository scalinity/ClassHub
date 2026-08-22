import { invoke } from "@tauri-apps/api/core";

export interface Meeting {
  weekday: number; // 1=Mon .. 7=Sun
  startTime: string; // "16:05"
  endTime: string; // "19:05"
}

export interface ClassInfo {
  id: number;
  displayName: string;
  color: string; // blue | orange | green | amber
  room: string;
  instructors: string;
  credits: number;
  folderName: string;
  folderPresent: boolean;
  /** Guides whose sources changed since generation (card badge). */
  staleGuides: number;
  meetings: Meeting[];
}

export function listClasses(): Promise<ClassInfo[]> {
  return invoke<ClassInfo[]>("list_classes");
}
