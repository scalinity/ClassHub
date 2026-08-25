import { useState } from "react";

import {
  chooseEffort,
  chooseModel,
  EFFORT_LEVELS,
  loadModels,
  removeKey,
  saveKey,
  type ChatSnapshot,
} from "@/lib/chat";

/**
 * SPEC §9 chat settings: the Anthropic key (Keychain-backed), the live model
 * list, and the effort ladder. Its own file — the transcript, this pane and
 * the markdown pipeline were three unrelated concerns in one module.
 */
export function SettingsPane({ chat }: { chat: ChatSnapshot }) {
  const [key, setKey] = useState("");
  const hasKey = chat.settings?.hasKey ?? false;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
      <p className="font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
        ANTHROPIC API KEY
      </p>
      <p className="mt-2 text-[12.5px] leading-relaxed text-muted-foreground">
        Kept in the macOS Keychain under the service name{" "}
        <code className="font-mono">classhub</code> — never in the database and
        never in a file. Asking bills this key directly, which is why a
        rate-limited synthesis job can never take the chat down with it.
      </p>
      <p className="mt-2 text-[12.5px] leading-relaxed text-muted-foreground">
        The item is saved readable by any app on this Mac. A Keychain item is
        otherwise locked to the exact binary that wrote it, and every rebuild in
        development is a new binary — which is what makes macOS ask for your
        login password again and again.
      </p>

      {hasKey ? (
        <div className="mt-3 flex items-center gap-2">
          <p className="flex-1 rounded-md border bg-muted/30 px-2.5 py-1.5 font-mono text-[11px] text-muted-foreground">
            sk-ant-••••••••••••••••
          </p>
          <button
            type="button"
            onClick={() => void removeKey()}
            className="cursor-pointer rounded-md border px-2.5 py-1.5 font-mono text-[10px] tracking-[0.12em] text-muted-foreground transition-colors hover:bg-muted hover:text-destructive focus-visible:outline-2 focus-visible:outline-ring"
          >
            REMOVE
          </button>
        </div>
      ) : (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const value = key;
            setKey("");
            void saveKey(value);
          }}
          className="mt-3 flex items-center gap-2"
        >
          <input
            type="password"
            value={key}
            autoFocus
            spellCheck={false}
            placeholder="sk-ant-…"
            onChange={(e) => setKey(e.target.value)}
            className="min-w-0 flex-1 rounded-md border bg-background px-2.5 py-1.5 font-mono text-[11px] outline-none focus:border-ring"
          />
          <button
            type="submit"
            disabled={key.trim() === ""}
            className="cursor-pointer rounded-md bg-primary px-2.5 py-1.5 font-mono text-[10px] tracking-[0.12em] text-primary-foreground transition-opacity hover:opacity-90 disabled:cursor-default disabled:opacity-30 focus-visible:outline-2 focus-visible:outline-ring"
          >
            SAVE
          </button>
        </form>
      )}

      <hr className="my-5" />

      <div className="flex items-baseline justify-between">
        <p className="font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
          MODEL
        </p>
        {hasKey && (
          <button
            type="button"
            onClick={() => void loadModels()}
            className="cursor-pointer rounded px-1.5 py-0.5 font-mono text-[10px] tracking-[0.12em] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
          >
            REFRESH
          </button>
        )}
      </div>

      {!hasKey ? (
        <p className="mt-2 text-[12.5px] text-muted-foreground">
          Save a key to load the live model list.
        </p>
      ) : chat.modelsError ? (
        <p className="mt-2 font-mono text-[10.5px] leading-relaxed text-destructive">
          {chat.modelsError}
        </p>
      ) : chat.models === null || chat.modelsLoading ? (
        <p className="mt-2 font-mono text-[10.5px] tracking-[0.14em] text-muted-foreground">
          LOADING MODELS…
        </p>
      ) : (
        <div className="mt-2.5 space-y-1">
          {chat.models.models.map((model) => {
            const selected = model.id === chat.models?.selected;
            return (
              <button
                key={model.id}
                type="button"
                onClick={() => void chooseModel(model.id)}
                className={
                  "flex w-full cursor-pointer items-center gap-2.5 rounded-md border px-2.5 py-1.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-ring " +
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
                  <span className="block truncate text-[12.5px] font-medium">
                    {model.displayName}
                  </span>
                  <span className="block truncate font-mono text-[10px] text-muted-foreground">
                    {model.id}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      )}

      <hr className="my-5" />

      <p className="font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
        EFFORT
      </p>
      <p className="mt-2 text-[12.5px] leading-relaxed text-muted-foreground">
        How many tokens an answer may spend — on reading, on reasoning, and on
        how many tools it calls before it commits. Levels are newer than some
        models: if a request comes back rejected, step down to the default.
      </p>
      <div className="mt-2.5 space-y-1">
        {EFFORT_LEVELS.map((level) => {
          const selected = (chat.settings?.effort ?? "") === level.id;
          return (
            <button
              key={level.id}
              type="button"
              onClick={() => void chooseEffort(level.id)}
              className={
                "flex w-full cursor-pointer items-center gap-2.5 rounded-md border px-2.5 py-1.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-ring " +
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
                  {level.label}
                </span>
                <span className="block text-[11.5px] leading-snug text-muted-foreground">
                  {level.note}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      <hr className="my-5" />

      <p className="font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
        WHAT ASKING CAN DO
      </p>
      <p className="mt-2 text-[12.5px] leading-relaxed text-muted-foreground">
        It reads your extracts, notes and study guides — and it can act: record
        and amend deadlines and grades, write notes into a class's Notes folder,
        start guide synthesis, and generate practice exams (both run as jobs in
        the Job Center). Chips marked ✎ changed something; overwritten or
        deleted data is kept in the audit log.
      </p>
      <p className="mt-2 text-[12.5px] leading-relaxed text-muted-foreground">
        The one thing it never does is move files: reorganizations are only
        proposals, and nothing moves until you approve each one.
      </p>
    </div>
  );
}
