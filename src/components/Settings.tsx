import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronLeft } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import { shortModel } from "@/lib/answer";
import {
  formatSyncedAt,
  getCanvasStatus,
  outcomeSummary,
  syncCanvas,
  useCanvasSync,
  type SyncProgress,
} from "@/lib/canvas";
import { openChatSettings, useChat, EFFORT_LEVELS } from "@/lib/chat";
import { queryClient } from "@/lib/query";
import {
  getAppSettings,
  JOB_EFFORT_COPY,
  JOB_MODEL_COPY,
  optionCopy,
  setAibhsRoot,
  setParakeetPython,
  setJobConcurrency,
  setJobEffort,
  setJobModel,
} from "@/lib/settings";
import {
  buttonFilledNeutral,
  buttonTextNeutral,
  errorLine,
  inputNeutral,
  meta,
  optionDot,
  optionDotIdle,
  optionDotSelected,
  optionRow,
  optionRowIdle,
  optionRowSelected,
} from "@/lib/styles";
import { sentence } from "@/lib/utils";

/**
 * SPEC §12 Settings view: the AIBHS library root and the job runner's model,
 * effort and concurrency. Chat's key/model/effort stay in the chat sidebar's
 * own pane — where a mid-conversation change is at hand — and are linked from
 * here rather than duplicated.
 */
export function SettingsScreen({ onBack }: { onBack: () => void }) {
  const { data: settings, error } = useQuery({
    queryKey: ["appSettings"],
    queryFn: getAppSettings,
  });
  const [actionError, setActionError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const apply = (change: Promise<void>, invalidateAll = false) => {
    setPending(true);
    setActionError(null);
    change
      .then(() => {
        setPending(false);
        if (invalidateAll) {
          // A new root changes what every query would answer.
          void queryClient.invalidateQueries();
        } else {
          void queryClient.invalidateQueries({ queryKey: ["appSettings"] });
        }
      })
      .catch((e) => {
        setActionError(String(e));
        setPending(false);
        void queryClient.invalidateQueries({ queryKey: ["appSettings"] });
      });
  };

  return (
    <main className="mx-auto max-w-4xl px-8 pt-12 pb-20 animate-in fade-in duration-200 motion-reduce:animate-none">
      <button type="button" onClick={onBack} className={`${buttonTextNeutral} -ml-2`}>
        <ChevronLeft size={14} aria-hidden />
        Dashboard
      </button>

      <header className="mt-4">
        <h1 className="text-display">Settings</h1>
      </header>

      {error ? (
        <p className={`${errorLine} mt-16 text-center`}>
          Couldn't load the settings: {String(error)}
        </p>
      ) : settings === undefined ? (
        <p className="mt-16 text-center text-body text-muted-foreground">Loading…</p>
      ) : (
        <>
          {actionError && <p className={`${errorLine} mt-6`}>{actionError}</p>}

          <Section
            title="Library"
            lead="Where the class folders live. ClassHub reads this tree directly
              and writes only inside its own folders — Study Guides, Notes, the
              inbox and the extract cache."
          >
            <PathForm
              key={settings.aibhsRoot}
              current={settings.aibhsRoot}
              present={settings.aibhsRootPresent}
              pending={pending}
              label="AIBHS root folder path"
              apply="Use this folder"
              missing="Folder not found: nothing can scan until this points at a real folder."
              hint="The folder must already exist — this picks a library, it never creates one. ~/ works."
              onApply={(path) => apply(setAibhsRoot(path), true)}
            />
          </Section>

          <Section
            title="Lecture transcription"
            lead="Recordings without a caption track are transcribed on this Mac
              with Parakeet, which ships inside LocalFlow. Nothing is uploaded,
              and no tokens are spent — but an update to LocalFlow can move the
              interpreter, so this is where to point it again."
          >
            <PathForm
              key={settings.parakeetPython}
              current={settings.parakeetPython}
              present={settings.parakeetPresent}
              pending={pending}
              label="Python interpreter for Parakeet"
              apply="Use this Python"
              missing="Interpreter not found: transcription will fail until this points at a real Python. Zoom's own transcripts still work."
              hint="A Python with parakeet-mlx installed. Clear the field to restore the default that ships with LocalFlow."
              allowEmpty
              onApply={(path) => apply(setParakeetPython(path))}
            />
          </Section>

          <Section
            title="Synthesis jobs"
            lead="Extraction, study guides, sorting and syllabus scans spawn
              Claude Code on the Max subscription with these settings. A change
              applies from the next job to start — running jobs keep what they
              started with."
          >
            <p className="mt-5 text-[15px] font-semibold">Model</p>
            <div className="mt-2 space-y-1">
              {settings.jobModels.map((id) => {
                const copy = optionCopy(JOB_MODEL_COPY, id);
                return (
                  <OptionRow
                    key={id}
                    label={copy.label}
                    note={copy.note}
                    selected={settings.jobModel === id}
                    disabled={pending}
                    onSelect={() => apply(setJobModel(id))}
                  />
                );
              })}
            </div>

            <p className="mt-6 text-[15px] font-semibold">Effort</p>
            <p className="mt-1 text-body text-muted-foreground">
              How long a job may read and reason before it writes.
            </p>
            <div className="mt-2 space-y-1">
              {settings.jobEfforts.map((id) => {
                const copy = optionCopy(JOB_EFFORT_COPY, id);
                return (
                  <OptionRow
                    key={id}
                    label={copy.label}
                    note={copy.note}
                    selected={settings.jobEffort === id}
                    disabled={pending}
                    onSelect={() => apply(setJobEffort(id))}
                  />
                );
              })}
            </div>

            <p className="mt-6 text-[15px] font-semibold">Concurrency</p>
            <p className="mt-1 text-body text-muted-foreground">
              How many jobs run at once. A semester master always runs alone,
              whatever this says.
            </p>
            <div className="mt-2.5 flex items-center gap-1.5">
              {Array.from({ length: settings.maxConcurrency }, (_, i) => i + 1).map(
                (count) => (
                  <button
                    key={count}
                    type="button"
                    disabled={pending}
                    aria-pressed={settings.jobConcurrency === count}
                    aria-label={`Run up to ${count} job${count === 1 ? "" : "s"} at once`}
                    onClick={() => apply(setJobConcurrency(count))}
                    className={
                      "flex size-8 cursor-pointer items-center justify-center rounded-md text-body tabular-nums transition-colors focus-visible:outline-2 focus-visible:outline-ring disabled:pointer-events-none " +
                      (settings.jobConcurrency === count
                        ? "bg-muted/70 font-semibold ring-1 ring-border"
                        : "text-muted-foreground hover:bg-muted/40")
                    }
                  >
                    {count}
                  </button>
                ),
              )}
              <span className={`ml-2 ${meta}`}>at once</span>
            </div>
          </Section>

          <CanvasSection />

          <ChatSection />
        </>
      )}
    </main>
  );
}

/**
 * SPEC §7.2 — Canvas.
 *
 * The section shows when data last came across and nothing more, because
 * nothing more is true: holding a session cookie is not the same as Canvas
 * still honouring it, so there is no connection whose health could be reported.
 * A green "connected" chip would stay green long after the session behind it
 * had expired.
 */
function CanvasSection() {
  const { data: status } = useQuery({
    queryKey: ["canvasStatus"],
    queryFn: getCanvasStatus,
  });
  const progress = useCanvasSync();
  const [refused, setRefused] = useState<string | null>(null);
  const running = progress !== null && !progress.done;

  async function start() {
    setRefused(null);
    try {
      await syncCanvas();
    } catch (e) {
      setRefused(String(e));
    }
  }

  return (
    <Section
      title="Canvas"
      lead="Canvas holds the authoritative version of each course — its own
        structure, its files, and its assignments with real due dates. ClassHub
        reads it through a Canvas window you sign in to, and keeps that
        session's own cookie in your Keychain so relaunching the app does not
        mean signing in again. Nothing is minted, and Canvas decides when the
        session ends. Syncing runs when you ask, and once on launch when the
        saved session is still live and the last sync is a day old — never on
        a schedule."
    >
      <dl className="mt-5 space-y-1.5 text-body">
        <div className="flex gap-3">
          <dt className={`w-24 shrink-0 ${meta}`}>Last sync</dt>
          <dd className="tabular-nums">{formatSyncedAt(status?.lastSyncedAt ?? null)}</dd>
        </div>
        <div className="flex gap-3">
          <dt className={`w-24 shrink-0 ${meta}`}>Classes</dt>
          <dd>
            {status === undefined
              ? "—"
              : status.classesLinked === 0
                ? "None matched yet"
                : `${status.classesLinked} matched to a course`}
          </dd>
        </div>
        <div className="flex gap-3">
          <dt className={`w-24 shrink-0 ${meta}`}>Reads</dt>
          <dd className="min-w-0 truncate text-muted-foreground">
            {status?.host ?? "ufl.instructure.com"} · never writes
          </dd>
        </div>
      </dl>

      <button
        type="button"
        disabled={running}
        onClick={() => void start()}
        className={`${buttonFilledNeutral} mt-4`}
      >
        {running ? "Syncing…" : "Sync all classes"}
      </button>

      {/* The sync never began — one is already running, started from a class
          workspace. Distinct from one that started and failed. */}
      {refused && <p className={`${errorLine} max-w-xl`}>Not started: {refused}</p>}
      <SyncReport progress={progress} />

      <p className="mt-4 max-w-xl text-body text-muted-foreground">
        You will be asked to sign in when Canvas ends the session, not every
        time you open the app, and never by the launch's own sync. Assignments
        arrive as deadline cards and files land in each class's inbox — both
        wait for your approval, the same as everything else that moves
        material. Announcements go straight to the class's Notices, and its
        Canvas Pages into the extract cache, where chat searches them.
      </p>
    </Section>
  );
}

/** What a sync is doing, and what it brought across when it finishes. */
function SyncReport({ progress }: { progress: SyncProgress | null }) {
  if (progress === null) return null;

  if (!progress.done) {
    return (
      <p className={`mt-3 flex items-center gap-2 ${meta}`}>
        <span
          aria-hidden
          className="size-1.5 shrink-0 rounded-full bg-foreground animate-pulse motion-reduce:animate-none"
        />
        {sentence(progress.stage)}
      </p>
    );
  }

  // A sync giving up because Canvas wants a sign-in it could not ask for is
  // the quiet outcome the launch's sync exists to allow — the next press will
  // ask. A note, not a stopped sync; any other failure on launch still is one.
  if (progress.error && progress.signInNeeded) {
    return (
      <p className="mt-3 max-w-xl text-body text-muted-foreground">
        {progress.launch ? "Not synced on launch" : "Not synced"}: {progress.error}
      </p>
    );
  }
  if (progress.error) {
    return (
      <p className={`${errorLine} max-w-xl`}>Sync stopped: {progress.error}</p>
    );
  }

  return (
    <dl className="mt-4 max-w-xl space-y-2.5">
      {progress.launch && <p className={meta}>Synced on launch</p>}
      {(progress.results ?? []).map((outcome) => (
        <div key={outcome.classId}>
          <dt className="flex items-baseline gap-2">
            <span className="text-body font-medium">{outcome.className}</span>
            {outcome.canvasCourse && (
              <span className="min-w-0 truncate text-fine text-muted-foreground">
                {outcome.canvasCourse}
              </span>
            )}
          </dt>
          <dd
            className={
              "text-meta leading-snug " +
              (outcome.error ? "text-destructive" : "text-muted-foreground")
            }
          >
            {outcomeSummary(outcome)}
          </dd>
          {outcome.notes.map((note, index) => (
            <dd
              key={`${outcome.classId}-${index}`}
              className="text-meta leading-snug text-muted-foreground/70"
            >
              {note}
            </dd>
          ))}
        </div>
      ))}
    </dl>
  );
}

function Section({
  title,
  lead,
  children,
}: {
  title: string;
  lead: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mt-12" aria-label={title}>
      <SectionHeading title={title} />
      <p className="mt-3 max-w-xl text-[14px] leading-[1.55] text-muted-foreground">
        {lead}
      </p>
      {children}
    </section>
  );
}

function OptionRow({
  label,
  note,
  selected,
  disabled,
  onSelect,
}: {
  label: string;
  note: string;
  selected: boolean;
  disabled: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      aria-pressed={selected}
      onClick={onSelect}
      className={`${optionRow} max-w-xl ${selected ? optionRowSelected : optionRowIdle}`}
    >
      <span
        aria-hidden
        className={`${optionDot} ${selected ? optionDotSelected : optionDotIdle}`}
      />
      <span className="min-w-0 flex-1">
        <span className="block text-body font-medium">{label}</span>
        <span className="block text-fine leading-snug text-muted-foreground">
          {note}
        </span>
      </span>
    </button>
  );
}

/** One filesystem path the user can repoint: the AIBHS root, or the
 *  interpreter on-device transcription runs through. */
function PathForm({
  current,
  present,
  pending,
  label,
  apply,
  missing,
  hint,
  allowEmpty = false,
  onApply,
}: {
  current: string;
  present: boolean;
  pending: boolean;
  label: string;
  apply: string;
  missing: string;
  hint: string;
  /** Whether clearing the field is itself a choice — it restores a default. */
  allowEmpty?: boolean;
  onApply: (path: string) => void;
}) {
  const [path, setPath] = useState(current);
  const changed = (allowEmpty || path.trim() !== "") && path.trim() !== current;

  return (
    <div className="mt-4 max-w-xl">
      {!present && <p className="mb-2 text-body text-destructive">{missing}</p>}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (changed && !pending) onApply(path.trim());
        }}
        className="flex items-center gap-2"
      >
        <input
          type="text"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          spellCheck={false}
          aria-label={label}
          className={`${inputNeutral} min-w-0 flex-1 font-mono text-code`}
        />
        <button
          type="submit"
          disabled={pending || !changed}
          className={buttonFilledNeutral}
        >
          {pending ? "Applying…" : apply}
        </button>
      </form>
      <p className="mt-2 text-body text-muted-foreground">{hint}</p>
    </div>
  );
}

/** Live status of the chat's own settings, linked to their home in the sidebar. */
function ChatSection() {
  const chat = useChat();
  const effortLabel =
    EFFORT_LEVELS.find((l) => l.id === (chat.settings?.effort ?? ""))?.label ??
    "Model default";

  return (
    <Section
      title="Chat"
      lead="The chat sidebar bills its own console API key, so a heavy
        synthesis job can never rate-limit a conversation. Its key, model and
        effort live in the sidebar's settings pane — at hand mid-conversation."
    >
      <dl className="mt-5 space-y-1.5 text-body">
        <div className="flex gap-3">
          <dt className={`w-20 shrink-0 ${meta}`}>API key</dt>
          <dd className={chat.settings?.hasKey ? "" : "text-destructive"}>
            {chat.settings?.hasKey ? "In the macOS Keychain" : "Not saved yet"}
          </dd>
        </div>
        <div className="flex gap-3">
          <dt className={`w-20 shrink-0 ${meta}`}>Model</dt>
          <dd className="min-w-0 truncate">
            {chat.settings?.model
              ? shortModel(chat.settings.model)
              : "Picked on first use"}
          </dd>
        </div>
        <div className="flex gap-3">
          <dt className={`w-20 shrink-0 ${meta}`}>Effort</dt>
          <dd>{effortLabel}</dd>
        </div>
      </dl>
      <button
        type="button"
        onClick={openChatSettings}
        className={`${buttonTextNeutral} mt-4 ring-1 ring-border`}
      >
        Open chat settings
      </button>
    </Section>
  );
}
