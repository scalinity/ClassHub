import type { Meeting } from "./classes";

export interface NextMeeting {
  meeting: Meeting;
  daysUntil: number; // 0 = today
  inSession: boolean;
}

/** ISO weekday of a JS Date: 1=Mon .. 7=Sun. */
export function isoWeekday(date: Date): number {
  return ((date.getDay() + 6) % 7) + 1;
}

/** Minutes since midnight for a "HH:MM" string. */
export function toMinutes(time: string): number {
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

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export function weekdayLabel(weekday: number): string {
  return WEEKDAYS[weekday - 1] ?? "?";
}

/** "16:05" -> "4:05 pm" (or without meridiem when told to omit it). */
export function formatTime(time: string, withMeridiem = true): string {
  const [h, m] = time.split(":").map(Number);
  const meridiem = h < 12 ? "am" : "pm";
  const hour12 = h % 12 === 0 ? 12 : h % 12;
  const base = `${hour12}:${String(m).padStart(2, "0")}`;
  return withMeridiem ? `${base} ${meridiem}` : base;
}

/** "11:45".."13:40" -> "11:45 am–1:40 pm"; same-meridiem ranges collapse: "4:05–7:05 pm". */
export function formatTimeRange(start: string, end: string): string {
  const sameMeridiem =
    (toMinutes(start) < 720) === (toMinutes(end) < 720);
  return `${formatTime(start, !sameMeridiem)}–${formatTime(end)}`;
}

export function relativeLabel(next: NextMeeting): string {
  if (next.inSession) return "in session";
  if (next.daysUntil === 0) return "today";
  if (next.daysUntil === 1) return "tomorrow";
  return `in ${next.daysUntil} days`;
}

/** A clock reading, "1:50 am". */
export function formatClock(date: Date): string {
  return date
    .toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })
    // ICU 72+ separates the day period with U+202F; match either space.
    .replace(/\s(AM|PM)$/, (_, p: string) => ` ${p.toLowerCase()}`);
}

/** A calendar day, "Sep 3"; with its year when it is not this year's. */
export function formatMonthDay(date: Date, now: Date = new Date()): string {
  return date.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    ...(date.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}

/** A moment, "Sep 3, 1:50 am" — the stamp under a guide, a sync, a note. */
export function formatStamp(date: Date): string {
  return `${formatMonthDay(date)}, ${formatClock(date)}`;
}

/**
 * Deadline due_at ("2026-09-03" or "2026-09-03T17:00") -> "Sep 3" (+ " at
 * 5:00 pm" when present). Parsed by parts: `new Date("YYYY-MM-DD")` is UTC
 * midnight, which renders as the previous local day in negative offsets.
 */
export function formatDueDate(dueAt: string): string {
  const [datePart, timePart] = dueAt.split("T");
  const [y, m, d] = datePart.split("-").map(Number);
  const label = formatMonthDay(new Date(y, (m || 1) - 1, d || 1));
  return timePart ? `${label} at ${formatTime(timePart.slice(0, 5))}` : label;
}

/** Local midnight of an ISO date(-time)'s date part, parsed by parts (see above). */
function localMidnight(iso: string): Date {
  const [y, m, d] = iso.split("T")[0].split("-").map(Number);
  return new Date(y, (m || 1) - 1, d || 1);
}

/** Whole days from today to the ISO date's day: 0 today, negative overdue. */
export function daysUntil(iso: string, now: Date = new Date()): number {
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  return Math.round((localMidnight(iso).getTime() - today.getTime()) / 86_400_000);
}

/**
 * Deadline label with the day named: "overdue since Aug 20", "today at
 * 11:59 pm", "tomorrow", "Wed, Aug 26 at 11:59 pm". The time follows when the
 * deadline carries one, except on an overdue one, where the day is the point.
 */
export function dueDayLabel(dueAt: string): string {
  const days = daysUntil(dueAt);
  const timePart = dueAt.split("T")[1];
  const time = timePart ? ` at ${formatTime(timePart.slice(0, 5))}` : "";
  if (days < 0) return `overdue since ${formatMonthDay(localMidnight(dueAt))}`;
  if (days === 0) return `today${time}`;
  if (days === 1) return `tomorrow${time}`;
  const weekday = localMidnight(dueAt).toLocaleDateString("en-US", {
    weekday: "short",
  });
  return `${weekday}, ${formatDueDate(dueAt)}`;
}

/** Today as YYYY-MM-DD in local time (backend stamps and date-input defaults). */
export function todayIso(now: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}
