import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AudioLines, Captions, Link2, X } from "lucide-react";

import {
  addLecture,
  classifySource,
  clearLectureProgress,
  dateFromFileName,
  lectureWeeks,
  useLectureProgress,
  type LectureProgress,
  type SourceKind,
  type WeekSlot,
} from "@/lib/lectures";
import { todayIso } from "@/lib/schedule";
import { setDropInterceptor } from "@/lib/sorter";
import { inputBase, monoAction } from "@/lib/styles";

/** What each kind of source will actually do, said before it happens. */
const SOURCE_HINT: Record<SourceKind, { icon: typeof Link2; text: string }> = {
  link: {
    icon: Link2,
    text: "Opens Zoom in a window — sign in there and the transcript is read from the page.",
  },
  caption: {
    icon: Captions,
    text: "Read directly. Zoom's own track keeps speaker names, which is the best input there is.",
  },
  media: {
    icon: AudioLines,
    text: "Transcribed on this Mac with Parakeet. No speaker names — the model doesn't separate voices.",
  },
  unknown: {
    icon: Link2,
    text: "Paste a Zoom recording link, or drop a caption track (.vtt, .srt, .txt) or a recording.",
  },
};

/**
 * SPEC §7.1 — the one way into lecture ingestion.
 *
 * A recording arrives as a link, a caption track, or bare audio, and the form's
 * job is to say which one it is looking at and what that will cost before
 * anything starts: reading a file is instant, transcribing is minutes, and a
 * link means signing in to Zoom.
 *
 * It also settles the one decision with judgement in it: which week the session
 * belongs to. That is what maps the lecture to a division of the course
 * (SPEC §8.5), so the form resolves it from the course's own schedule, shows
 * what it resolved to and what that division is, and lets it be changed. A
 * course that publishes no dates for its weeks — Applied Generative AI's Parts
 * name week ranges and no days — gets no default and is asked outright, each
 * week's option naming the Part it feeds.
 */
export function AddLecture({
  classId,
  onClose,
}: {
  classId: number;
  onClose: () => void;
}) {
  const [source, setSource] = useState("");
  const [date, setDate] = useState<string | null>(null);
  // null until touched, so resolving the week from the date never fights an
  // edit; "" is the deliberate "let the sorter decide", which is a different
  // answer from not having answered.
  const [week, setWeek] = useState<number | "" | null>(null);
  const [title, setTitle] = useState("");
  const [digest, setDigest] = useState(true);
  const [submitted, setSubmitted] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const progress = useLectureProgress(classId);
  // `submitted` covers the gap before the first progress event — without it the
  // form re-renders enabled between the click and the backend's first stage
  // line, and a second click starts a second ingestion. `progress` covers the
  // reverse: reopening the dialog on a run already in flight.
  const running = (submitted || progress !== null) && !progress?.done;

  // A dropped file fills the form instead of starting a sort. Assigned during
  // render (the contract `setDropTarget` already uses) and cleared by `close`.
  setDropInterceptor((paths) => {
    const first = paths[0];
    if (first) {
      setSource(first);
      setError(null);
    }
  });

  const close = () => {
    // A finished run's entry is done with; an unfinished one is kept, so
    // reopening the dialog rejoins the run in progress rather than offering a
    // form that would start a second one.
    if (progress?.done) clearLectureProgress(classId);
    onClose();
    // Last, and after the store write above: clearing the interceptor before a
    // synchronous notify lets this component's own re-render re-arm it on the
    // way out, leaving a stale claim that swallows every later drop.
    setDropInterceptor(null);
  };

  const kind = classifySource(source);
  const hint = SOURCE_HINT[kind];
  // Derived until touched, so picking a file fills the date but never fights
  // an edit.
  const resolvedDate = date ?? dateFromFileName(source) ?? todayIso();
  // Keyed on the date: the week follows from it, so changing the date
  // re-resolves rather than leaving a stale answer standing.
  const { data: weeks } = useQuery({
    queryKey: ["lectureWeeks", classId, resolvedDate],
    queryFn: () => lectureWeeks(classId, resolvedDate),
    placeholderData: (prev) => prev,
  });
  const slots = weeks?.slots ?? [];
  const resolvedWeek = week === null ? (weeks?.defaultWeek ?? null) : week;
  const slot = slots.find((s) => s.week === resolvedWeek) ?? null;
  // A course that groups its weeks — Applied Generative AI's three Parts —
  // names none of them, so its folders are bare `Week NN` and the option has
  // to say what the week feeds. A week's own folder already does.
  const optionLabel = (s: WeekSlot) =>
    s.unitKind === "week" ? s.folder : `${s.folder} · ${s.unitName}`;
  // The course published no dates to measure the session against, so the
  // week is asked for outright (SPEC §8.5). Keyed on the dates rather than
  // on the default: a date the field cannot parse resolves to no default
  // either, and that is not the course's doing.
  const asked = slots.length > 0 && slots.every((s) => s.meetsOn === null);

  const submit = () => {
    setError(null);
    setSubmitted(true);
    addLecture({
      classId,
      source: source.trim(),
      week: slot?.week ?? null,
      date: resolvedDate,
      title: title.trim() === "" ? null : title.trim(),
      digest,
    }).catch((e) => {
      setSubmitted(false);
      setError(String(e));
    });
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Add lecture"
      onKeyDown={(e) => {
        // Closable at any point: the run continues in the background, and a
        // modal that cannot be dismissed takes the whole app with it if a run
        // ever ends without reporting.
        if (e.key === "Escape") close();
      }}
      className="fixed inset-0 z-50 flex items-center justify-center bg-background/70 p-6 backdrop-blur-[2px] animate-in fade-in duration-150 motion-reduce:animate-none"
    >
      <div className="w-full max-w-lg overflow-hidden rounded-xl border bg-card shadow-2xl">
        <header className="flex h-12 items-center gap-2.5 border-b px-3 pl-4">
          <p className="flex-1 font-mono text-[11px] tracking-[0.18em] text-(--accent)">
            ADD LECTURE
          </p>
          <button
            type="button"
            aria-label="Close"
            onClick={close}
            className="shrink-0 cursor-pointer rounded p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
          >
            <X size={14} aria-hidden />
          </button>
        </header>

        {progress?.done ? (
          <Outcome progress={progress} onClose={close} />
        ) : running ? (
          <Running stage={progress?.stage ?? "Starting…"} />
        ) : (
          <div className="space-y-5 p-5">
            <Field label="RECORDING">
              <input
                type="text"
                value={source}
                onChange={(e) => setSource(e.target.value)}
                placeholder="Paste a Zoom link, or drop a file here"
                aria-label="Recording link or file"
                autoFocus
                spellCheck={false}
                className={`${inputBase} w-full`}
              />
              <p className="mt-1.5 flex items-start gap-1.5 text-[12px] text-muted-foreground">
                <hint.icon
                  size={12}
                  aria-hidden
                  className="mt-0.5 shrink-0 text-muted-foreground/70"
                />
                {hint.text}
              </p>
            </Field>

            <div className="grid grid-cols-2 gap-4">
              <Field label="SESSION DATE">
                <input
                  type="date"
                  value={resolvedDate}
                  onChange={(e) => setDate(e.target.value)}
                  aria-label="Session date"
                  className={`${inputBase} w-full`}
                />
              </Field>
              <Field label="WEEK">
                <select
                  value={resolvedWeek ?? ""}
                  onChange={(e) =>
                    setWeek(e.target.value === "" ? "" : Number(e.target.value))
                  }
                  disabled={slots.length === 0}
                  aria-label="Week"
                  className={`${inputBase} w-full disabled:opacity-50`}
                >
                  <option value="">
                    {slots.length === 0
                      ? "No schedule published"
                      : "Sort it into a week"}
                  </option>
                  {slots.map((s) => (
                    <option key={s.week} value={s.week}>
                      {optionLabel(s)}
                    </option>
                  ))}
                </select>
              </Field>
            </div>

            <p className="-mt-3 text-[12px] leading-relaxed text-muted-foreground">
              {slot ? (
                <>
                  Filed under{" "}
                  <span className="font-mono text-[11px] text-foreground">
                    Weeks/{slot.folder}
                  </span>
                  , feeding <span className="text-foreground">{slot.unitName}</span>
                  . Wrong week? Change it here, or refile it later — the map
                  follows the file.
                </>
              ) : slots.length === 0 ? (
                "This course publishes no weekly schedule, so ClassHub can't place the session itself. It goes to the inbox and the sorter proposes a week from what the lecture covers."
              ) : asked ? (
                "This course publishes no dates for its weeks, so pick the one the session fell in. Left unpicked, it goes to the inbox and the sorter proposes a week."
              ) : (
                "Pick the week this session belongs to. It goes to the inbox until you do, and the sorter proposes one from what the lecture covers."
              )}
            </p>

            <Field label="TITLE — OPTIONAL">
              <input
                type="text"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder="Lecture"
                aria-label="Lecture title"
                className={`${inputBase} w-full`}
              />
            </Field>

            <label className="flex cursor-pointer items-start gap-2.5">
              <input
                type="checkbox"
                checked={digest}
                disabled={slot === null}
                onChange={(e) => setDigest(e.target.checked)}
                className="mt-0.5 size-3.5 shrink-0 cursor-pointer accent-(--accent)"
              />
              <span className="text-[13px]">
                Write a session document
                <span className="mt-0.5 block text-[12px] text-muted-foreground">
                  {slot === null
                    ? "Available once the session has a week — ClassHub files it first, then you can distill it."
                    : "Distills the transcript into a session summary, plus the note this week's study guide is built from."}
                </span>
              </span>
            </label>

            {error && (
              <p className="font-mono text-[11px] text-destructive">
                NOT STARTED — {error}
              </p>
            )}

            <div className="flex justify-end gap-2 border-t pt-4">
              <button
                type="button"
                onClick={close}
                className={`${monoAction} text-muted-foreground hover:bg-muted`}
              >
                CANCEL
              </button>
              <button
                type="button"
                onClick={submit}
                disabled={source.trim() === ""}
                className={`${monoAction} text-(--accent) hover:bg-(--accent)/12 disabled:pointer-events-none disabled:opacity-40`}
              >
                ADD LECTURE
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <p className="mb-1.5 font-mono text-[10px] tracking-[0.14em] text-muted-foreground">
        {label}
      </p>
      {children}
    </div>
  );
}

function Running({ stage }: { stage: string }) {
  return (
    <div className="px-5 py-12 text-center">
      <div className="flex items-center justify-center gap-2">
        <span
          aria-hidden
          className="size-1.5 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
        />
        <p className="font-mono text-[11px] tracking-[0.18em] text-(--accent)">
          {stage.toUpperCase()}
        </p>
      </div>
      <p className="mx-auto mt-3 max-w-xs text-[13px] text-muted-foreground">
        Transcribing a full lecture takes a few minutes. This keeps running if
        you close the window — the Job Center has the rest.
      </p>
    </div>
  );
}

function Outcome({
  progress,
  onClose,
}: {
  progress: LectureProgress;
  onClose: () => void;
}) {
  const result = progress.result;
  return (
    <div className="p-5">
      {result ? (
        <>
          <p className="font-mono text-[11px] tracking-[0.18em] text-(--accent)">
            TRANSCRIPT FILED
          </p>
          <p className="mt-2 font-mono text-[12px] break-all">{result.relPath}</p>
          <p className="mt-3 text-[13px] text-muted-foreground">
            {result.routedToInbox
              ? "It's in the inbox — ClassHub is proposing a week for it now, and the proposal appears above the materials list."
              : result.digestJobId !== null
                ? `The session document and ${result.unitName ?? "this week"}'s study note are being written. Watch them in the Job Center.`
                : `It feeds ${result.unitName ?? "no division yet"}. Distill it to write the note that guide is built from.`}
          </p>
          {result.digestError && (
            <p className="mt-3 font-mono text-[11px] text-destructive">
              NO SESSION DOCUMENT — {result.digestError}
            </p>
          )}
          {result.speakers.length > 0 && (
            <p className="mt-3 text-[12px] text-muted-foreground">
              Speakers: {result.speakers.join(", ")}
            </p>
          )}
        </>
      ) : (
        <>
          <p className="font-mono text-[11px] tracking-[0.18em] text-destructive">
            COULD NOT ADD THE LECTURE
          </p>
          <p className="mt-2 text-[13px] text-muted-foreground">
            {progress.error}
          </p>
        </>
      )}
      <div className="mt-5 flex justify-end border-t pt-4">
        <button
          type="button"
          onClick={onClose}
          className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
        >
          DONE
        </button>
      </div>
    </div>
  );
}
