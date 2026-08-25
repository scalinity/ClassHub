import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronLeft } from "lucide-react";

import { openChatSettings, useChat, EFFORT_LEVELS } from "@/lib/chat";
import { queryClient } from "@/lib/query";
import {
  getAppSettings,
  JOB_EFFORT_COPY,
  JOB_MODEL_COPY,
  optionCopy,
  setAibhsRoot,
  setJobConcurrency,
  setJobEffort,
  setJobModel,
} from "@/lib/settings";
import { monoActionNeutral } from "@/lib/styles";

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
    <main className="mx-auto max-w-4xl px-8 pt-16 pb-20 animate-in fade-in duration-200">
      <button
        type="button"
        onClick={onBack}
        className="flex cursor-pointer items-center gap-1 font-mono text-[11px] tracking-[0.18em] text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
      >
        <ChevronLeft size={12} aria-hidden />
        DASHBOARD
      </button>

      <header className="mt-6">
        <p className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          CONFIGURATION
        </p>
        <h1 className="mt-2 text-[28px] font-semibold leading-tight tracking-tight">
          Settings
        </h1>
      </header>

      {error ? (
        <p className="mt-16 text-center font-mono text-xs text-destructive">
          FAILED TO LOAD SETTINGS — {String(error)}
        </p>
      ) : settings === undefined ? (
        <p className="mt-16 text-center font-mono text-xs text-muted-foreground">
          LOADING…
        </p>
      ) : (
        <>
          {actionError && (
            <p className="mt-6 font-mono text-[11px] text-destructive">
              ✕ {actionError}
            </p>
          )}

          <Section
            title="LIBRARY"
            lead="Where the class folders live. ClassHub reads this tree directly
              and writes only inside its own folders — Study Guides, Notes, the
              inbox and the extract cache."
          >
            <RootForm
              key={settings.aibhsRoot}
              current={settings.aibhsRoot}
              present={settings.aibhsRootPresent}
              pending={pending}
              onApply={(path) => apply(setAibhsRoot(path), true)}
            />
          </Section>

          <Section
            title="SYNTHESIS JOBS"
            lead="Extraction, study guides, sorting and syllabus scans spawn
              Claude Code on the Max subscription with these settings. A change
              applies from the next job to start — running jobs keep what they
              started with."
          >
            <p className="mt-4 font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
              MODEL
            </p>
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

            <p className="mt-6 font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
              EFFORT
            </p>
            <p className="mt-1.5 text-[12.5px] leading-relaxed text-muted-foreground">
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

            <p className="mt-6 font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
              CONCURRENCY
            </p>
            <p className="mt-1.5 text-[12.5px] leading-relaxed text-muted-foreground">
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
                      "flex size-8 cursor-pointer items-center justify-center rounded-md border font-mono text-[12px] transition-colors focus-visible:outline-2 focus-visible:outline-ring disabled:pointer-events-none " +
                      (settings.jobConcurrency === count
                        ? "border-foreground/25 bg-muted/60 font-semibold"
                        : "border-transparent text-muted-foreground hover:bg-muted/40")
                    }
                  >
                    {count}
                  </button>
                ),
              )}
              <span className="ml-2 font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
                AT ONCE
              </span>
            </div>
          </Section>

          <ChatSection />
        </>
      )}
    </main>
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
      <div className="border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          {title}
        </h2>
      </div>
      <p className="mt-3 max-w-xl text-[12.5px] leading-relaxed text-muted-foreground">
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
      className={
        "flex w-full max-w-xl cursor-pointer items-center gap-2.5 rounded-md border px-2.5 py-1.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-ring disabled:pointer-events-none " +
        (selected
          ? "border-foreground/25 bg-muted/60"
          : "border-transparent hover:bg-muted/40")
      }
    >
      <span
        aria-hidden
        className={
          "size-1.5 shrink-0 rounded-full " +
          (selected ? "bg-foreground" : "bg-muted-foreground/25")
        }
      />
      <span className="min-w-0 flex-1">
        <span className="block font-mono text-[10px] font-medium tracking-[0.12em]">
          {label}
        </span>
        <span className="block text-[11.5px] leading-snug text-muted-foreground">
          {note}
        </span>
      </span>
    </button>
  );
}

function RootForm({
  current,
  present,
  pending,
  onApply,
}: {
  current: string;
  present: boolean;
  pending: boolean;
  onApply: (path: string) => void;
}) {
  const [path, setPath] = useState(current);
  const changed = path.trim() !== "" && path.trim() !== current;

  return (
    <div className="mt-4 max-w-xl">
      {!present && (
        <p className="mb-2 font-mono text-[11px] text-destructive">
          FOLDER NOT FOUND — nothing can scan until this points at a real
          folder.
        </p>
      )}
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
          aria-label="AIBHS root folder path"
          className="h-8 min-w-0 flex-1 rounded-md border bg-transparent px-2.5 font-mono text-[12px] focus-visible:outline-2 focus-visible:outline-ring"
        />
        <button
          type="submit"
          disabled={pending || !changed}
          className={`${monoActionNeutral} bg-primary px-2.5 text-primary-foreground hover:opacity-90 disabled:pointer-events-none disabled:opacity-30`}
        >
          {pending ? "APPLYING…" : "USE THIS FOLDER"}
        </button>
      </form>
      <p className="mt-2 text-[11.5px] leading-relaxed text-muted-foreground">
        The folder must already exist — this picks a library, it never creates
        one. ~/ works.
      </p>
    </div>
  );
}

/** Live status of the chat's own settings, linked to their home in the sidebar. */
function ChatSection() {
  const chat = useChat();
  const effortLabel =
    EFFORT_LEVELS.find((l) => l.id === (chat.settings?.effort ?? ""))?.label ??
    "MODEL DEFAULT";

  return (
    <Section
      title="CHAT"
      lead="The chat sidebar bills its own console API key, so a heavy
        synthesis job can never rate-limit a conversation. Its key, model and
        effort live in the sidebar's settings pane — at hand mid-conversation."
    >
      <dl className="mt-4 space-y-1.5 font-mono text-[11px]">
        <div className="flex gap-3">
          <dt className="w-20 shrink-0 tracking-[0.14em] text-muted-foreground/70">
            API KEY
          </dt>
          <dd className={chat.settings?.hasKey ? "" : "text-destructive"}>
            {chat.settings?.hasKey ? "IN THE MACOS KEYCHAIN" : "NOT SAVED YET"}
          </dd>
        </div>
        <div className="flex gap-3">
          <dt className="w-20 shrink-0 tracking-[0.14em] text-muted-foreground/70">
            MODEL
          </dt>
          <dd className="min-w-0 truncate">
            {chat.settings?.model?.toUpperCase() ?? "PICKED ON FIRST USE"}
          </dd>
        </div>
        <div className="flex gap-3">
          <dt className="w-20 shrink-0 tracking-[0.14em] text-muted-foreground/70">
            EFFORT
          </dt>
          <dd>{effortLabel}</dd>
        </div>
      </dl>
      <button
        type="button"
        onClick={openChatSettings}
        className={`${monoActionNeutral} mt-4 border px-2.5 text-muted-foreground hover:bg-muted hover:text-foreground`}
      >
        OPEN CHAT SETTINGS
      </button>
    </Section>
  );
}
