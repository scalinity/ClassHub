import { useQuery } from "@tanstack/react-query";
import { memo, useMemo, useRef, useState } from "react";
import {
  ArrowUp,
  ChevronDown,
  ChevronRight,
  MessagesSquare,
  Plus,
  Settings2,
  Square,
  X,
} from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import "katex/dist/katex.min.css";

import { FileViewer } from "@/components/FileViewer";
import {
  formatArgs,
  renderAnswer,
  shortModel,
} from "@/lib/answer";
import { listClasses, type ClassInfo } from "@/lib/classes";
import {
  chooseEffort,
  chooseModel,
  EFFORT_LEVELS,
  closeChat,
  formatSessionDate,
  isWriteTool,
  loadModels,
  newChat,
  removeKey,
  requestFileView,
  saveKey,
  selectSession,
  sendChat,
  stopChat,
  toggleChat,
  togglePicker,
  toggleSettings,
  toolLabel,
  useChat,
  type ChatItem,
  type ChatSnapshot,
} from "@/lib/chat";
import { iconAction } from "@/lib/styles";

/**
 * The answer's document register, expressed as element rules so a rendered
 * answer sits in the same type system as the rest of the app. Citations are
 * anchored `button.cite` elements the markdown pass injects.
 */
const PROSE = [
  "text-[13.5px] leading-[1.65]",
  "[&_p]:my-2 [&_p:first-child]:mt-0",
  "[&_h1]:mt-4 [&_h1]:mb-1.5 [&_h1]:text-[15px] [&_h1]:font-semibold",
  "[&_h2]:mt-4 [&_h2]:mb-1.5 [&_h2]:text-[14px] [&_h2]:font-semibold",
  "[&_h3]:mt-3.5 [&_h3]:mb-1 [&_h3]:font-mono [&_h3]:text-[10.5px] [&_h3]:tracking-[0.16em] [&_h3]:uppercase [&_h3]:text-muted-foreground",
  "[&_ul]:my-2 [&_ul]:list-disc [&_ul]:pl-4 [&_ol]:my-2 [&_ol]:list-decimal [&_ol]:pl-4",
  "[&_li]:my-1 [&_li]:marker:text-muted-foreground/50",
  "[&_strong]:font-semibold [&_em]:italic",
  "[&_a]:underline [&_a]:decoration-dotted",
  "[&_blockquote]:my-2 [&_blockquote]:border-l-2 [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground",
  "[&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:border [&_pre]:bg-muted/40 [&_pre]:p-2.5 [&_pre]:font-mono [&_pre]:text-[11.5px]",
  "[&_:not(pre)>code]:rounded [&_:not(pre)>code]:bg-muted [&_:not(pre)>code]:px-1 [&_:not(pre)>code]:py-0.5 [&_:not(pre)>code]:font-mono [&_:not(pre)>code]:text-[11.5px]",
  "[&_table]:my-2 [&_table]:w-full [&_table]:border-collapse",
  "[&_th]:border [&_th]:px-2 [&_th]:py-1 [&_th]:text-left [&_th]:font-mono [&_th]:text-[10px] [&_th]:uppercase [&_th]:tracking-[0.1em] [&_th]:text-muted-foreground",
  "[&_td]:border [&_td]:px-2 [&_td]:py-1 [&_td]:align-top",
  "[&_hr]:my-3.5",
  "[&_.katex]:text-[1.02em]",
  "[&_.katex-display]:my-2.5 [&_.katex-display]:overflow-x-auto [&_.katex-display]:overflow-y-hidden [&_.katex-display]:py-0.5",
  "[&_button.cite]:cursor-pointer [&_button.cite]:rounded [&_button.cite]:px-0.5 [&_button.cite]:font-mono [&_button.cite]:text-[11px] [&_button.cite]:text-(--cite) [&_button.cite]:underline [&_button.cite]:decoration-dotted [&_button.cite]:underline-offset-2 [&_button.cite]:hover:bg-muted",
].join(" ");

/**
 * Openers, drawn three at a time so the panel doesn't greet you with the same
 * three questions for the rest of the semester. `{class}` is filled from a
 * class you actually have.
 */
const STARTERS = [
  "What's due next, and what should I work on first?",
  "Which study guides are stale, and what changed under them?",
  "What did the most recent module of {class} cover?",
  "What's the through-line across the modules in {class}?",
  "Quiz me on the trickiest ideas in {class}",
  "Explain the hardest concept in {class} in plain terms",
  "Where do my notes go further than the {class} slides?",
  "What should I review before the next {class} deadline?",
  "Make me a practice exam for the latest module of {class}",
  "Distill the newest module of {class} into a note I can skim",
];

function pickStarters(classes: readonly ClassInfo[]): string[] {
  const name = classes[Math.floor(Math.random() * classes.length)]?.displayName;
  const pool = name
    ? STARTERS.map((s) => s.replace("{class}", name))
    : STARTERS.filter((s) => !s.includes("{class}"));
  return [...pool].sort(() => Math.random() - 0.5).slice(0, 3);
}

/**
 * SPEC §9 — the ask panel: a right-edge overlay (⌘J) over whatever you were
 * reading, so a question never costs you your place. Answers stream as
 * documents rather than chat bubbles, tool calls collapse into mono chips, and
 * every cited path opens the file it names.
 */
export function ChatSidebar() {
  const chat = useChat();
  const { data: classes } = useQuery({
    queryKey: ["classes"],
    queryFn: listClasses,
  });

  return (
    <>
      {!chat.open && (
        <button
          type="button"
          aria-label="Ask ClassHub"
          onClick={toggleChat}
          className="fixed right-4 bottom-4 z-40 flex h-9 cursor-pointer items-center gap-2 rounded-full border bg-card px-3.5 font-mono text-[11px] tracking-[0.14em] shadow-md transition-shadow hover:shadow-lg focus-visible:outline-2 focus-visible:outline-ring"
        >
          <MessagesSquare size={12} aria-hidden />
          ASK
          <span aria-hidden className="text-muted-foreground/60">
            ⌘J
          </span>
        </button>
      )}

      {chat.open && <Panel chat={chat} classes={classes ?? []} />}

      {chat.viewFile && (
        <FileViewer
          classId={chat.viewFile.classId}
          file={chat.viewFile}
          onClose={() => requestFileView(null)}
        />
      )}
    </>
  );
}

function Panel({
  chat,
  classes,
}: {
  chat: ChatSnapshot;
  classes: readonly ClassInfo[];
}) {
  const [draft, setDraft] = useState("");
  // Stick to the newest line unless the reader scrolls up; the ref callback
  // runs every render, so streaming keeps the view pinned without effects.
  const stick = useRef(true);

  const session = chat.sessions.find((s) => s.id === chat.sessionId);
  const submit = (text: string) => {
    if (text.trim() === "" || chat.streaming) return;
    setDraft("");
    void sendChat(text);
  };

  return (
    <section
      aria-label="Ask ClassHub"
      onKeyDown={(e) => {
        if (e.key === "Escape") closeChat();
      }}
      className="fixed inset-y-0 right-0 z-40 flex w-[min(30rem,90vw)] flex-col border-l bg-card shadow-2xl animate-in fade-in slide-in-from-right-4 duration-200 motion-reduce:animate-none"
    >
      <header className="relative flex h-11 shrink-0 items-center gap-1 border-b px-2.5">
        <p className="mr-auto pl-1 font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          ASK · CLASSHUB
        </p>
        <button
          type="button"
          aria-label="New conversation"
          title="New conversation"
          onClick={newChat}
          className={iconAction}
        >
          <Plus size={14} aria-hidden />
        </button>
        <button
          type="button"
          aria-expanded={chat.pickerOpen}
          aria-label="Past conversations"
          onClick={togglePicker}
          className="flex shrink-0 cursor-pointer items-center gap-1 rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
        >
          HISTORY
          <ChevronDown
            size={11}
            aria-hidden
            className={
              "transition-transform duration-150 motion-reduce:transition-none " +
              (chat.pickerOpen ? "rotate-180" : "")
            }
          />
        </button>
        <button
          type="button"
          aria-expanded={chat.settingsOpen}
          aria-label="Chat settings"
          title="Chat settings"
          onClick={toggleSettings}
          className={iconAction}
        >
          <Settings2 size={14} aria-hidden />
        </button>
        <button
          type="button"
          aria-label="Close chat"
          onClick={closeChat}
          className={iconAction}
        >
          <X size={14} aria-hidden />
        </button>

        {chat.pickerOpen && (
          <div className="absolute inset-x-0 top-11 z-10 max-h-80 overflow-y-auto border-b bg-card py-1 shadow-lg">
            {chat.sessions.length === 0 ? (
              <p className="px-4 py-6 text-center text-[13px] text-muted-foreground">
                No conversations yet.
              </p>
            ) : (
              chat.sessions.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  onClick={() => void selectSession(s.id)}
                  className={
                    "flex w-full cursor-pointer items-center gap-2.5 px-3 py-1.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-ring " +
                    (s.id === chat.sessionId ? "bg-muted/70" : "hover:bg-muted/40")
                  }
                >
                  <span className="w-12 shrink-0 font-mono text-[10px] tracking-[0.1em] text-muted-foreground">
                    {formatSessionDate(s.createdAt)}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-[13px]">
                    {s.title}
                  </span>
                  <span className="shrink-0 font-mono text-[10px] text-muted-foreground/60">
                    {s.messageCount}
                  </span>
                </button>
              ))
            )}
          </div>
        )}
      </header>

      {chat.settingsOpen ? (
        <SettingsPane chat={chat} />
      ) : (
        <>
          <div
            onScroll={(e) => {
              const el = e.currentTarget;
              stick.current =
                el.scrollHeight - el.scrollTop - el.clientHeight < 32;
            }}
            ref={(el) => {
              if (el && stick.current) el.scrollTop = el.scrollHeight;
            }}
            className="min-h-0 flex-1 overflow-y-auto px-4 py-4"
          >
            {chat.items.length === 0 ? (
              <EmptyState
                hasKey={chat.settings?.hasKey ?? false}
                classes={classes}
                onAsk={submit}
              />
            ) : (
              <>
                {session && (
                  <p className="mb-2 font-mono text-[10px] tracking-[0.16em] text-muted-foreground/60">
                    {formatSessionDate(session.createdAt)} · CONVERSATION #
                    {session.id}
                  </p>
                )}
                {chat.items.map((item, index) => (
                  <Turn
                    key={index}
                    item={item}
                    classes={classes}
                    streaming={chat.streaming && index === chat.items.length - 1}
                  />
                ))}
                {chat.streaming &&
                  chat.items[chat.items.length - 1]?.kind !== "answer" && (
                    <p className="mt-2 animate-pulse font-mono text-[10.5px] tracking-[0.14em] text-muted-foreground motion-reduce:animate-none">
                      WORKING…
                    </p>
                  )}
                {!chat.streaming &&
                  chat.suggestionsFor === chat.sessionId &&
                  chat.suggestions.length > 0 && (
                    <div className="mt-5 border-t pt-3">
                      <p className="font-mono text-[10px] tracking-[0.16em] text-muted-foreground/60">
                        FOLLOW UP
                      </p>
                      <div className="mt-2 space-y-1.5">
                        {chat.suggestions.map((text) => (
                          <button
                            key={text}
                            type="button"
                            onClick={() => submit(text)}
                            className="group flex w-full cursor-pointer items-baseline gap-2 rounded-md border border-dashed px-2.5 py-2 text-left transition-colors hover:border-solid hover:bg-muted/50 focus-visible:outline-2 focus-visible:outline-ring"
                          >
                            <span
                              aria-hidden
                              className="shrink-0 font-mono text-[11px] text-muted-foreground/50 transition-colors group-hover:text-foreground"
                            >
                              ›
                            </span>
                            <span className="text-[12.5px] leading-snug text-muted-foreground transition-colors group-hover:text-foreground">
                              {text}
                            </span>
                          </button>
                        ))}
                      </div>
                    </div>
                  )}
              </>
            )}
          </div>

          <form
            onSubmit={(e) => {
              e.preventDefault();
              submit(draft);
            }}
            className="shrink-0 border-t p-2.5"
          >
            {chat.error && (
              <p className="mb-2 px-1 font-mono text-[10.5px] leading-relaxed text-destructive">
                {chat.error}
              </p>
            )}
            <div className="flex items-end gap-1.5 rounded-lg border bg-background px-2.5 py-2 focus-within:border-ring">
              <textarea
                rows={1}
                autoFocus
                value={draft}
                placeholder="Ask about your classes…"
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    submit(draft);
                  }
                }}
                // Grow with the question, up to about eight lines.
                ref={(el) => {
                  if (!el) return;
                  el.style.height = "auto";
                  el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
                }}
                className="flex-1 resize-none bg-transparent text-[13.5px] leading-relaxed outline-none placeholder:text-muted-foreground/60"
              />
              {chat.streaming ? (
                <button
                  type="button"
                  aria-label="Stop answering"
                  onClick={stopChat}
                  className="shrink-0 cursor-pointer rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
                >
                  <Square size={12} aria-hidden fill="currentColor" />
                </button>
              ) : (
                <button
                  type="submit"
                  aria-label="Send"
                  disabled={draft.trim() === ""}
                  className="shrink-0 cursor-pointer rounded-md bg-primary p-1.5 text-primary-foreground transition-opacity hover:opacity-90 disabled:cursor-default disabled:opacity-25 focus-visible:outline-2 focus-visible:outline-ring"
                >
                  <ArrowUp size={12} aria-hidden strokeWidth={2.5} />
                </button>
              )}
            </div>
            <div className="mt-1.5 flex items-center justify-between px-1 font-mono text-[10px] tracking-[0.12em] text-muted-foreground/60">
              <span className="truncate">
                {chat.settings?.model
                  ? shortModel(chat.settings.model)
                  : "DIRECT API · NO MODEL SET"}
              </span>
              <span className="shrink-0 pl-2">⏎ SEND · ⇧⏎ NEWLINE</span>
            </div>
          </form>
        </>
      )}
    </section>
  );
}

function EmptyState({
  hasKey,
  classes,
  onAsk,
}: {
  hasKey: boolean;
  classes: readonly ClassInfo[];
  onAsk: (text: string) => void;
}) {
  // A fresh draw each time the panel opens; the class list settling re-draws.
  const starters = useMemo(() => pickStarters(classes), [classes]);
  if (!hasKey) {
    return (
      <div className="mt-6">
        <p className="font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
          NO API KEY YET
        </p>
        <p className="mt-2.5 text-[13px] leading-relaxed text-muted-foreground">
          Asking runs on the Anthropic API with your own key, stored in the macOS
          Keychain. It is deliberately separate from the Max subscription the
          synthesis jobs use, so a rate-limited job never costs you an answer.
        </p>
        <button
          type="button"
          onClick={toggleSettings}
          className="mt-4 cursor-pointer rounded-md border px-3 py-1.5 font-mono text-[10.5px] tracking-[0.14em] transition-colors hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring"
        >
          ADD KEY
        </button>
      </div>
    );
  }

  return (
    <div className="mt-6">
      <p className="font-mono text-[10.5px] tracking-[0.16em] text-muted-foreground">
        READS YOUR MATERIAL · ACTS ON IT
      </p>
      <p className="mt-2.5 text-[13px] leading-relaxed text-muted-foreground">
        Questions are answered from the extracted slides, notebooks, notes and
        study guides in your classes, with every file it read cited inline. Ask
        and it also records deadlines and grades, writes notes, and kicks off
        guide synthesis or a practice exam. File moves stay proposals you
        approve.
      </p>
      <div className="mt-4 space-y-1.5">
        {starters.map((text) => (
          <button
            key={text}
            type="button"
            onClick={() => onAsk(text)}
            className="block w-full cursor-pointer rounded-md border border-dashed px-2.5 py-2 text-left text-[12.5px] leading-snug text-muted-foreground transition-colors hover:border-solid hover:bg-muted/50 hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
          >
            {text}
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * Memoized because the streaming store notifies on every delta: without it a
 * single token re-parses every answer in the transcript, KaTeX and all.
 */
const Turn = memo(function Turn({
  item,
  classes,
  streaming,
}: {
  item: ChatItem;
  classes: readonly ClassInfo[];
  streaming: boolean;
}) {
  switch (item.kind) {
    case "question":
      return (
        <p className="mt-5 border-l-2 border-foreground/30 pl-2.5 text-[13.5px] font-medium leading-snug whitespace-pre-wrap first:mt-0">
          {item.text}
        </p>
      );
    case "tool":
      return <ToolChip item={item} />;
    case "thinking":
      return <ThinkingBlock item={item} />;
    case "answer":
      return <Answer item={item} classes={classes} streaming={streaming} />;
    case "notice":
      return (
        <p className="mt-2.5 rounded-md border border-destructive/40 bg-destructive/5 px-2.5 py-2 font-mono text-[10.5px] leading-relaxed text-destructive">
          {item.text}
        </p>
      );
  }
});

function Answer({
  item,
  classes,
  streaming,
}: {
  item: Extract<ChatItem, { kind: "answer" }>;
  classes: readonly ClassInfo[];
  streaming: boolean;
}) {
  // `item.text` only advances at settled block boundaries, so the parse runs
  // once per block rather than once per streamed token.
  const html = useMemo(
    () => renderAnswer(item.text, classes),
    [item.text, classes],
  );

  return (
    <div
      onClick={(e) => {
        const target = e.target as HTMLElement;
        const cite = target.closest("button.cite");
        if (cite instanceof HTMLElement) {
          openCitation(cite);
          return;
        }
        // An answer's links are the model's, not the app's: the webview must
        // never navigate off the app document, so they open in the browser.
        const anchor = target.closest("a");
        if (anchor instanceof HTMLAnchorElement) {
          e.preventDefault();
          const href = anchor.getAttribute("href") ?? "";
          if (/^https?:\/\//i.test(href)) void openUrl(href).catch(() => {});
        }
      }}
      className={`mt-2.5 ${PROSE}`}
    >
      {item.text !== "" && <div dangerouslySetInnerHTML={{ __html: html }} />}
      {item.tail.length > 0 && (
        // The unparsed tail: each delta is its own element, so React only
        // ever appends — settled text never re-mounts and never re-fades.
        <p className="my-2 whitespace-pre-wrap first:mt-0">
          {item.tail.map((chunk, index) => (
            <span
              key={index}
              className="animate-in fade-in duration-700 motion-reduce:animate-none"
            >
              {chunk}
            </span>
          ))}
          {streaming && <Caret />}
        </p>
      )}
    </div>
  );
}

function Caret() {
  return (
    <span
      aria-hidden
      className="ml-0.5 inline-block h-[0.95em] w-[2px] translate-y-[1px] animate-pulse bg-current align-baseline motion-reduce:animate-none"
    />
  );
}

/**
 * Reasoning, folded away. It is a summary the API writes rather than the
 * model's raw thought, so it earns a line — not the page the answer gets.
 */
function ThinkingBlock({
  item,
}: {
  item: Extract<ChatItem, { kind: "thinking" }>;
}) {
  const lines = item.text.split("\n").filter((line) => line.trim() !== "");
  const preview = item.done ? lines[0] : lines[lines.length - 1];

  return (
    <details className="group mt-2.5">
      <summary className="flex cursor-pointer list-none items-center gap-2 rounded-md px-1 py-1 text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring [&::-webkit-details-marker]:hidden">
        <span
          aria-hidden
          className={
            "shrink-0 font-mono text-[11px] " +
            (item.done ? "" : "animate-pulse motion-reduce:animate-none")
          }
        >
          ◇
        </span>
        <span className="shrink-0 font-mono text-[10px] font-medium tracking-[0.12em]">
          THINKING
        </span>
        <span className="min-w-0 flex-1 truncate text-[11.5px] italic opacity-70">
          {preview ?? "…"}
        </span>
        <ChevronRight
          size={10}
          aria-hidden
          className="shrink-0 opacity-40 transition-transform duration-150 group-open:rotate-90 motion-reduce:transition-none"
        />
      </summary>
      <p className="mt-1.5 mb-1 ml-1 border-l pl-3 text-[11.5px] leading-relaxed whitespace-pre-wrap text-muted-foreground">
        {item.text}
      </p>
    </details>
  );
}

function ToolChip({ item }: { item: Extract<ChatItem, { kind: "tool" }> }) {
  const pending = item.summary === undefined;

  return (
    <details className="group mt-2.5">
      <summary className="flex cursor-pointer list-none items-center gap-2 rounded-md border bg-muted/30 px-2 py-1.5 transition-colors hover:bg-muted/60 focus-visible:outline-2 focus-visible:outline-ring [&::-webkit-details-marker]:hidden">
        <span
          aria-hidden
          className={
            "shrink-0 font-mono text-[11px] " +
            (pending
              ? "animate-pulse text-foreground motion-reduce:animate-none"
              : item.isError
                ? "text-destructive"
                : "text-muted-foreground/50")
          }
        >
          {/* ✎ marks a chip that changed something; » only ever read. */}
          {isWriteTool(item) ? "✎" : "»"}
        </span>
        <span className="shrink-0 font-mono text-[10px] font-medium tracking-[0.12em]">
          {toolLabel(item.name)}
        </span>
        <span
          className={
            "min-w-0 flex-1 truncate text-[11.5px] " +
            (item.isError ? "text-destructive" : "text-muted-foreground")
          }
        >
          {pending ? "running…" : item.summary}
        </span>
        <ChevronRight
          size={10}
          aria-hidden
          className="shrink-0 text-muted-foreground/40 transition-transform duration-150 group-open:rotate-90 motion-reduce:transition-none"
        />
      </summary>
      <div className="mt-1.5 mb-1 ml-1 space-y-1.5 border-l pl-3">
        <pre className="whitespace-pre-wrap font-mono text-[10.5px] leading-relaxed text-foreground/70">
          {formatArgs(item.input)}
        </pre>
        {item.detail !== undefined && (
          <pre className="max-h-56 overflow-y-auto whitespace-pre-wrap font-mono text-[10.5px] leading-relaxed text-muted-foreground">
            {item.detail}
          </pre>
        )}
      </div>
    </details>
  );
}

function SettingsPane({ chat }: { chat: ChatSnapshot }) {
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

function openCitation(cite: HTMLElement) {
  const classId = Number(cite.dataset.class);
  const relPath = cite.dataset.path;
  if (!Number.isFinite(classId) || !relPath) return;
  requestFileView({
    classId,
    relPath,
    name: cite.dataset.name ?? relPath,
    kind: cite.dataset.kind ?? "md",
  });
}

