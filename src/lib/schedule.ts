import type { Meeting } from "./classes";

export interface NextMeeting {
  meeting: Meeting;
  daysUntil: number; // 0 = today
  inSession: boolean;
}

/** ISO weekday of a JS Date: 1=Mon .. 7=Sun. */
function isoWeekday(date: Date): number {
  return ((date.getDay() + 6) % 7) + 1;
}

/** Minutes since midnight for a "HH:MM" string. */
function toMinutes(time: string): number {
  const [h, m] = time.split(":").map(Number);
  return h * 60 + m;
}

export function nextMeeting(
  meetings: Meeting[],
  now: Date = new Date(),
): NextMeeting | null {
  const today = isoWeekday(now);
  const nowMinutes = now.getHours() * 60 + now.getMinutes();

  let best: NextMeeting | null = null;
  for (const meeting of meetings) {
    let daysUntil = (meeting.weekday - today + 7) % 7;
    let inSession = false;
    if (daysUntil === 0) {
      if (nowMinutes >= toMinutes(meeting.endTime)) {
        daysUntil = 7; // already over today; next week
      } else {
        inSession = nowMinutes >= toMinutes(meeting.startTime);
      }
    }
    const candidate = { meeting, daysUntil, inSession };
    if (
      best === null ||
      daysUntil < best.daysUntil ||
      (daysUntil === best.daysUntil &&
        toMinutes(meeting.startTime) < toMinutes(best.meeting.startTime))
    ) {
      best = candidate;
    }
  }
  return best;
}

const WEEKDAYS = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];

export function weekdayLabel(weekday: number): string {
  return WEEKDAYS[weekday - 1] ?? "?";
}

/** "16:05" -> "4:05 PM" (or without meridiem when told to omit it). */
export function formatTime(time: string, withMeridiem = true): string {
  const [h, m] = time.split(":").map(Number);
  const meridiem = h < 12 ? "AM" : "PM";
  const hour12 = h % 12 === 0 ? 12 : h % 12;
  const base = `${hour12}:${String(m).padStart(2, "0")}`;
  return withMeridiem ? `${base} ${meridiem}` : base;
}

/** "11:45".."13:40" -> "11:45 AM–1:40 PM"; same-meridiem ranges collapse: "4:05–7:05 PM". */
export function formatTimeRange(start: string, end: string): string {
  const sameMeridiem =
    (toMinutes(start) < 720) === (toMinutes(end) < 720);
  return `${formatTime(start, !sameMeridiem)}–${formatTime(end)}`;
}

export function relativeLabel(next: NextMeeting): string {
  if (next.inSession) return "IN SESSION";
  if (next.daysUntil === 0) return "TODAY";
  if (next.daysUntil === 1) return "TOMORROW";
  return `IN ${next.daysUntil} DAYS`;
}
