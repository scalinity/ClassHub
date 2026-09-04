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
import {
  buttonFilledNeutral,
  buttonTextNeutral,
  errorLine,
  inputNeutral,
  optionDot,
  optionDotIdle,
  optionDotSelected,
  optionRow,
  optionRowIdle,
  optionRowSelected,
} from "@/lib/styles";

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
      <h3 className="text-[15px] font-semibold">Anthropic API key</h3>
      <p className="mt-2 text-body text-muted-foreground">
        Kept in the macOS Keychain under the service name{" "}
        <code className="font-mono text-code">classhub</code> — never in the
        database and never in a file. Asking bills this key directly, which is
        why a rate-limited synthesis job can never take the chat down with it.
      </p>
      <p className="mt-2 text-body text-muted-foreground">
        The item is saved readable by any app on this Mac. A Keychain item is
        otherwise locked to the exact binary that wrote it, and every rebuild in
        development is a new binary — which is what makes macOS ask for your
        login password again and again.
      </p>

      {hasKey ? (
        <div className="mt-3 flex items-center gap-2">
          <p className="flex-1 rounded-md bg-muted/40 px-2.5 py-1.5 font-mono text-code text-muted-foreground ring-1 ring-border">
            sk-ant-••••••••••••••••
          </p>
          <button
            type="button"
            onClick={() => void removeKey()}
            className={`${buttonTextNeutral} hover:text-destructive`}
          >
            Remove
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
            className={`${inputNeutral} min-w-0 flex-1 font-mono text-code`}
          />
          <button
            type="submit"
            disabled={key.trim() === ""}
            className={buttonFilledNeutral}
          >
            Save
          </button>
        </form>
      )}

      <div className="mt-8 flex items-baseline justify-between">
        <h3 className="text-[15px] font-semibold">Model</h3>
        {hasKey && (
          <button
            type="button"
            onClick={() => void loadModels()}
            className={buttonTextNeutral}
          >
            Refresh
          </button>
        )}
      </div>

      {!hasKey ? (
        <p className="mt-2 text-body text-muted-foreground">
          Save a key to load the live model list.
        </p>
      ) : chat.modelsError ? (
        <p className={errorLine}>{chat.modelsError}</p>
      ) : chat.models === null || chat.modelsLoading ? (
        <p className="mt-2 text-meta text-muted-foreground">Loading models…</p>
      ) : (
        <div className="mt-2.5 space-y-1">
          {chat.models.models.map((model) => {
            const selected = model.id === chat.models?.selected;
            return (
              <button
                key={model.id}
                type="button"
                onClick={() => void chooseModel(model.id)}
                className={`${optionRow} ${selected ? optionRowSelected : optionRowIdle}`}
              >
                <span
                  aria-hidden
                  className={`${optionDot} ${selected ? optionDotSelected : optionDotIdle}`}
                />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-body font-medium">
                    {model.displayName}
                  </span>
                  <span className="block truncate font-mono text-fine text-muted-foreground">
                    {model.id}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      )}

      <h3 className="mt-8 text-[15px] font-semibold">Effort</h3>
      <p className="mt-2 text-body text-muted-foreground">
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
              className={`${optionRow} ${selected ? optionRowSelected : optionRowIdle}`}
            >
              <span
                aria-hidden
                className={`${optionDot} ${selected ? optionDotSelected : optionDotIdle}`}
              />
              <span className="min-w-0 flex-1">
                <span className="block text-body font-medium">{level.label}</span>
                <span className="block text-fine leading-snug text-muted-foreground">
                  {level.note}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      <h3 className="mt-8 text-[15px] font-semibold">What asking can do</h3>
      <p className="mt-2 text-body text-muted-foreground">
        It reads your extracts, notes and study guides — and it can act: record
        and amend deadlines and grades, write notes into a class's Notes folder,
        start a study guide, and write practice exams (both run as jobs). Chips
        marked ✎ changed something; overwritten or deleted data is kept in the
        audit log.
      </p>
      <p className="mt-2 text-body text-muted-foreground">
        The one thing it never does is move files: reorganizations are only
        proposals, and nothing moves until you approve each one.
      </p>
    </div>
  );
}
