import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";

import { formatClock, formatMonthDay, todayIso } from "@/lib/schedule";
import { openClassId } from "@/lib/sorter";
import { sentence } from "@/lib/utils";

export interface ChatSettings {
  hasKey: boolean;
  model: string | null;
  /** `output_config.effort`; null leaves the level to the model. */
  effort: string | null;
}

/** SPEC §9 effort ladder, cheapest first. */
export const EFFORT_LEVELS = [
  { id: "", label: "Model default", note: "Whatever the model does unasked — high, on current models." },
  { id: "low", label: "Low", note: "Fewest tokens and fastest. Quick lookups, one or two reads." },
  { id: "medium", label: "Medium", note: "Balanced. Solid on routine questions about a module." },
  { id: "high", label: "High", note: "Reads more widely before answering. The API's own default." },
  { id: "xhigh", label: "Extra high", note: "Extended searching across classes. Slower, costlier." },
  { id: "max", label: "Max", note: "No ceiling on reasoning. For the hardest synthesis questions." },
] as const;

export interface ModelOption {
  id: string;
  displayName: string;
  /** The largest max_tokens the model accepts, when it publishes one. */
  maxTokens: number | null;
}

export interface ModelList {
  models: ModelOption[];
  selected: string;
}

export interface SessionInfo {
  id: number;
  title: string;
  createdAt: number;
  messageCount: number;
}

interface StoredMessage {
  id: number;
  role: string;
  /** JSON array of Messages-API content blocks. */
  content: string;
  createdAt: number;
}

/**
 * One rendered line of a transcript. Live streaming and rebuilt history both
 * produce this shape, so a relaunched session renders identically.
 */
export type ChatItem =
  | { kind: "question"; text: string }
  /**
   * `text` holds the markdown blocks that are finished and safe to parse;
   * `tail` is the unparsed remainder, split into the chunks it arrived in so
   * each can fade in on its own.
   */
  | {
      kind: "answer";
      text: string;
      tail: readonly string[];
      /** Whether `text` ends inside an unclosed code fence — carried so each
       *  settle counts only the newly settled slice. */
      openFence?: boolean;
    }
  | { kind: "thinking"; text: string; done: boolean }
  | {
      kind: "tool";
      toolId: string;
      name: string;
      /** JSON arguments, shown behind the chip's disclosure. */
      input: string;
      summary?: string;
      detail?: string;
      isError?: boolean;
      /** Served by the backend rather than mirrored here. */
      isWrite?: boolean;
    }
  | { kind: "notice"; text: string };

/** A cited file the reader asked to open (chat is global, so is the viewer). */
export interface ViewRequest {
  classId: number;
  relPath: string;
  name: string;
  kind: string;
}

export interface ChatSnapshot {
  open: boolean;
  settingsOpen: boolean;
  pickerOpen: boolean;
  sessionId: number | null;
  items: readonly ChatItem[];
  streaming: boolean;
  sessions: readonly SessionInfo[];
  settings: ChatSettings | null;
  models: ModelList | null;
  modelsLoading: boolean;
  modelsError: string | null;
  error: string | null;
  viewFile: ViewRequest | null;
  /** Model-written next questions, offered under the answer they follow. */
  suggestions: readonly string[];
  suggestionsFor: number | null;
}

interface ChatEventPayload {
  sessionId: number;
  kind:
    | "started"
    | "text"
    | "thinking"
    | "thinking_end"
    | "tool"
    | "tool_result"
    | "suggestions"
    | "done"
    | "error";
  text?: string;
  toolId?: string;
  name?: string;
  input?: string;
  summary?: string;
  detail?: string;
  isError?: boolean;
  isWrite?: boolean;
  suggestions?: string[];
}

// --- External store (push-based Tauri events; no useEffect per workspace rules) ---

let snapshot: ChatSnapshot = {
  open: false,
  settingsOpen: false,
  pickerOpen: false,
  sessionId: null,
  items: [],
  streaming: false,
  sessions: [],
  settings: null,
  models: null,
  modelsLoading: false,
  modelsError: null,
  error: null,
  viewFile: null,
  suggestions: [],
  suggestionsFor: null,
};

const storeListeners = new Set<() => void>();
/** Transcript per session: live turns and loaded history land in the same list. */
const itemsBySession = new Map<number, readonly ChatItem[]>();
const streamingSessions = new Set<number>();

function emitChange(patch: Partial<ChatSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  for (const notify of storeListeners) notify();
}

/** Publishes a session's transcript only when it is the one on screen. */
function commit(sessionId: number) {
  if (sessionId !== snapshot.sessionId) return;
  emitChange({
    items: itemsBySession.get(sessionId) ?? [],
    streaming: streamingSessions.has(sessionId),
  });
}

function setItems(sessionId: number, items: readonly ChatItem[]) {
  itemsBySession.set(sessionId, items);
  commit(sessionId);
}

function pushItem(sessionId: number, item: ChatItem) {
  setItems(sessionId, [...(itemsBySession.get(sessionId) ?? []), item]);
}

/** Text deltas extend the open answer; anything else starts a new one. */
function appendAnswer(sessionId: number, chunk: string) {
  const items = itemsBySession.get(sessionId) ?? [];
  const last = items[items.length - 1];
  const open: Extract<ChatItem, { kind: "answer" }> =
    last?.kind === "answer" ? last : { kind: "answer", text: "", tail: [] };
  const grown = settle({ ...open, tail: [...open.tail, chunk] });
  if (last?.kind === "answer") {
    setItems(sessionId, [...items.slice(0, -1), grown]);
  } else {
    pushItem(sessionId, grown);
  }
}

/**
 * Markdown can only be parsed a whole block at a time, so a finished block
 * moves into `text` and the rest keeps streaming as plain chunks. A block
 * boundary inside a fence is not one: settling there would render half a code
 * block, so the fence has to close first.
 */
function settle(
  item: Extract<ChatItem, { kind: "answer" }>,
): Extract<ChatItem, { kind: "answer" }> {
  const tail = item.tail.join("");
  const cut = tail.lastIndexOf("\n\n");
  if (cut < 0) return item;
  const settling = tail.slice(0, cut + 2);
  // Fences are counted over the newly settled slice and the parity carried on
  // the item, rather than rescanning the whole answer each time — that was
  // O(n) per settle and O(n²) across a long answer.
  // Returned unchanged when the fence is still open: `text` has not advanced,
  // so its parity has not either, and this slice gets counted again next time
  // along with whatever arrived after it.
  const stillOpen =
    (item.openFence ?? false) !== (countFences(settling) % 2 !== 0);
  if (stillOpen) return item;
  const rest = tail.slice(cut + 2);
  return {
    kind: "answer",
    text: item.text + settling,
    tail: rest === "" ? [] : [rest],
    openFence: false,
  };
}

function countFences(s: string): number {
  let count = 0;
  for (let i = s.indexOf("```"); i !== -1; i = s.indexOf("```", i + 3)) count++;
  return count;
}

/** Nothing is still arriving, so the whole answer can be parsed as markdown. */
function settleAll(sessionId: number) {
  const items = itemsBySession.get(sessionId) ?? [];
  const last = items[items.length - 1];
  if (last?.kind !== "answer" || last.tail.length === 0) return;
  setItems(sessionId, [
    ...items.slice(0, -1),
    { kind: "answer", text: last.text + last.tail.join(""), tail: [] },
  ]);
}

/** Thinking streams into one collapsible block per bout of reasoning. */
function appendThinking(sessionId: number, chunk: string) {
  const items = itemsBySession.get(sessionId) ?? [];
  const last = items[items.length - 1];
  if (last?.kind === "thinking" && !last.done) {
    setItems(sessionId, [
      ...items.slice(0, -1),
      { ...last, text: last.text + chunk },
    ]);
  } else {
    pushItem(sessionId, { kind: "thinking", text: chunk, done: false });
  }
}

function closeThinking(sessionId: number) {
  const items = itemsBySession.get(sessionId) ?? [];
  const last = items[items.length - 1];
  if (last?.kind !== "thinking") return;
  setItems(sessionId, [...items.slice(0, -1), { ...last, done: true }]);
}

function resolveTool(sessionId: number, event: ChatEventPayload) {
  const items = itemsBySession.get(sessionId) ?? [];
  setItems(
    sessionId,
    items.map((item) =>
      item.kind === "tool" && item.toolId === event.toolId
        ? {
            ...item,
            summary: event.summary,
            detail: event.detail,
            isError: event.isError,
          }
        : item,
    ),
  );
}

function handleEvent(event: ChatEventPayload) {
  const { sessionId } = event;
  switch (event.kind) {
    case "started":
      streamingSessions.add(sessionId);
      // Last answer's follow-ups no longer follow anything.
      emitChange({ suggestions: [], suggestionsFor: null });
      // The question rides the event, so a brand-new session renders from the
      // stream alone — even before `send_chat` returns its id.
      pushItem(sessionId, { kind: "question", text: event.text ?? "" });
      void refreshSessions();
      break;
    case "text":
      appendAnswer(sessionId, event.text ?? "");
      break;
    case "thinking":
      appendThinking(sessionId, event.text ?? "");
      break;
    case "thinking_end":
      closeThinking(sessionId);
      break;
    case "suggestions":
      emitChange({
        suggestions: event.suggestions ?? [],
        suggestionsFor: sessionId,
      });
      break;
    case "tool":
      pushItem(sessionId, {
        kind: "tool",
        toolId: event.toolId ?? "",
        name: event.name ?? "tool",
        input: event.input ?? "{}",
        isWrite: event.isWrite,
      });
      break;
    case "tool_result":
      resolveTool(sessionId, event);
      break;
    case "error":
      pushItem(sessionId, { kind: "notice", text: event.text ?? "Unknown error" });
      break;
    case "done":
      streamingSessions.delete(sessionId);
      closeThinking(sessionId);
      settleAll(sessionId);
      commit(sessionId);
      void refreshSessions();
      break;
  }
}

/** Mirrors `MAX_DETAIL_CHARS` in chat.rs, so a reopened disclosure shows what
 *  the live one did rather than the larger persisted body. */
const DETAIL_CHARS = 4_000;

/** Mirrors `truncate` in db.rs: counts characters, appends the ellipsis. */
function truncateChars(s: string, max: number): string {
  const chars = [...s];
  return chars.length <= max ? s : `${chars.slice(0, max).join("")}…`;
}

/** Mirrors `Outcome::ok`: every tool's first output line works as its summary. */
function summaryLine(text: string): string {
  return truncateChars((text.split("\n", 1)[0] ?? "").trim(), 140);
}

/** Persisted blocks → transcript items, folding tool results onto their call. */
function itemsFromHistory(messages: StoredMessage[]): ChatItem[] {
  const items: ChatItem[] = [];
  const toolPositions = new Map<string, number>();

  for (const message of messages) {
    // `JSON.parse` returns `any`, so the declared type was an assertion, not a
    // check — and the catch only covered malformed JSON. A row that is valid
    // JSON but not an array would throw on iteration and take out the whole
    // transcript render rather than skipping one message.
    let parsed: unknown;
    try {
      parsed = JSON.parse(message.content);
    } catch {
      continue;
    }
    if (!Array.isArray(parsed)) continue;
    const blocks = parsed as Array<Record<string, unknown>>;
    for (const block of blocks) {
      if (block.type === "text") {
        const text = String(block.text ?? "");
        items.push(
          message.role === "user"
            ? { kind: "question", text }
            : { kind: "answer", text, tail: [] },
        );
      } else if (block.type === "thinking") {
        // Blocks kept only for their signature carry no readable text.
        const text = String(block.thinking ?? "");
        if (text !== "") items.push({ kind: "thinking", text, done: true });
      } else if (block.type === "tool_use") {
        toolPositions.set(String(block.id), items.length);
        items.push({
          kind: "tool",
          toolId: String(block.id),
          name: String(block.name ?? "tool"),
          input: JSON.stringify(block.input ?? {}),
        });
      } else if (block.type === "tool_result") {
        const position = toolPositions.get(String(block.tool_use_id));
        const call = position === undefined ? undefined : items[position];
        if (position === undefined || call?.kind !== "tool") continue;
        const isError = block.is_error === true;
        const full = String(block.content ?? "");
        items[position] = {
          ...call,
          // Derived exactly as tools.rs Outcome does, so a chip rebuilt from
          // history reads identically to the live one: chars not UTF-16 units,
          // trimmed, ellipsis when cut — and for an error the whole message is
          // the summary rather than its first line.
          summary: isError ? truncateChars(full, 140) : summaryLine(full),
          detail: truncateChars(full, DETAIL_CHARS),
          isError,
        };
      }
    }
  }
  return items;
}

async function refreshSessions() {
  try {
    const sessions = await invoke<SessionInfo[]>("list_chat_sessions");
    emitChange({ sessions });
  } catch (e) {
    emitChange({ error: String(e) });
  }
}

let initialized = false;
function init() {
  if (initialized) return;
  initialized = true;
  void listen<ChatEventPayload>("chat-event", (e) => handleEvent(e.payload));
  void refreshSettings();
  void refreshSessions();
  // Global shortcut (SPEC §12). A module-level listener keeps this out of
  // component effects; Escape stays local to the panel so it never steals the
  // key from an open viewer overlay.
  window.addEventListener("keydown", (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "j") {
      event.preventDefault();
      toggleChat();
    }
  });
}
init();

export function useChat(): ChatSnapshot {
  return useSyncExternalStore(
    (cb) => {
      storeListeners.add(cb);
      return () => storeListeners.delete(cb);
    },
    () => snapshot,
  );
}

// --- Actions ---

export function toggleChat() {
  const open = !snapshot.open;
  emitChange({ open, pickerOpen: false });
  // Opening picks up the most recent conversation rather than a blank page.
  if (open && snapshot.sessionId === null && snapshot.sessions.length > 0) {
    void selectSession(snapshot.sessions[0].id);
  }
}

export function closeChat() {
  emitChange({ open: false, pickerOpen: false, settingsOpen: false });
}

export function togglePicker() {
  emitChange({ pickerOpen: !snapshot.pickerOpen });
}

export function toggleSettings() {
  const settingsOpen = !snapshot.settingsOpen;
  emitChange({ settingsOpen, pickerOpen: false });
  if (settingsOpen && snapshot.settings?.hasKey && snapshot.models === null) {
    void loadModels();
  }
}

/** The Settings screen's link into the chat pane (key, model, effort live there). */
export function openChatSettings() {
  emitChange({ open: true, settingsOpen: true, pickerOpen: false });
  if (snapshot.settings?.hasKey && snapshot.models === null) {
    void loadModels();
  }
}

export function newChat() {
  emitChange({ sessionId: null, items: [], streaming: false, pickerOpen: false, error: null });
}

export async function selectSession(id: number) {
  emitChange({
    sessionId: id,
    pickerOpen: false,
    error: null,
    items: itemsBySession.get(id) ?? [],
    streaming: streamingSessions.has(id),
  });
  if (itemsBySession.has(id)) return;
  try {
    const messages = await invoke<StoredMessage[]>("chat_history", {
      sessionId: id,
    });
    setItems(id, itemsFromHistory(messages));
  } catch (e) {
    emitChange({ error: String(e) });
  }
}

export async function sendChat(text: string) {
  const question = text.trim();
  if (question === "") return;
  emitChange({ error: null });
  try {
    const sessionId = await invoke<number>("send_chat", {
      sessionId: snapshot.sessionId,
      text: question,
      today: todayLabel(),
      // YYYY-MM-DD in local time — stamps practice file names backend-side.
      todayIso: todayIso(),
      // The open workspace rides along as context for this turn only.
      classId: openClassId(),
    });
    if (snapshot.sessionId !== sessionId) {
      emitChange({
        sessionId,
        items: itemsBySession.get(sessionId) ?? [],
        streaming: streamingSessions.has(sessionId),
      });
    }
  } catch (e) {
    emitChange({ error: String(e) });
  }
}

export function stopChat() {
  if (snapshot.sessionId === null) return;
  void invoke("stop_chat", { sessionId: snapshot.sessionId });
}

export async function refreshSettings() {
  try {
    const settings = await invoke<ChatSettings>("chat_settings");
    emitChange({ settings });
  } catch (e) {
    emitChange({ error: String(e) });
  }
}

export async function loadModels() {
  emitChange({ modelsLoading: true, modelsError: null });
  try {
    const models = await invoke<ModelList>("list_chat_models");
    // Loading also resolves the default on first use, so mirror what the
    // backend settled on.
    emitChange({
      models,
      modelsLoading: false,
      settings: snapshot.settings
        ? { ...snapshot.settings, model: models.selected }
        : null,
    });
  } catch (e) {
    emitChange({ modelsError: String(e), modelsLoading: false });
  }
}

export async function saveKey(key: string) {
  emitChange({ error: null });
  try {
    await invoke("save_chat_key", { key });
  } catch (e) {
    emitChange({ error: String(e) });
    return;
  }
  await refreshSettings();
  await loadModels();
}

export async function removeKey() {
  emitChange({ error: null });
  try {
    await invoke("delete_chat_key");
    emitChange({ models: null, modelsError: null });
  } catch (e) {
    emitChange({ error: String(e) });
  }
  await refreshSettings();
}

export async function chooseEffort(effort: string) {
  try {
    await invoke("set_chat_effort", { effort });
    emitChange({
      settings: snapshot.settings
        ? { ...snapshot.settings, effort: effort === "" ? null : effort }
        : null,
    });
  } catch (e) {
    emitChange({ modelsError: String(e) });
  }
}

export async function chooseModel(model: string) {
  try {
    await invoke("set_chat_model", { model });
    emitChange({
      models: snapshot.models ? { ...snapshot.models, selected: model } : null,
      settings: snapshot.settings ? { ...snapshot.settings, model } : null,
    });
  } catch (e) {
    emitChange({ modelsError: String(e) });
  }
}

export function requestFileView(view: ViewRequest | null) {
  emitChange({ viewFile: view });
}

// --- Formatting ---

/** The system prompt's "today" (std Rust cannot format a local date). */
function todayLabel(): string {
  return new Date().toLocaleDateString("en-US", {
    weekday: "long",
    month: "long",
    day: "numeric",
    year: "numeric",
  });
}

export function formatSessionDate(unixSec: number): string {
  const created = new Date(unixSec * 1000);
  const sameDay = created.toDateString() === new Date().toDateString();
  return sameDay ? formatClock(created) : formatMonthDay(created);
}

/** `search_material` → `Search material`. */
export function toolLabel(name: string): string {
  return sentence(name.replace(/_/g, " "));
}

/** M8 write tools — their chips carry a pen glyph instead of the read arrows. */
/**
 * Live turns carry the flag from the backend, which owns the list. Rebuilt
 * history has no event to carry it, so it falls back to the persisted name.
 */
const WRITE_TOOLS = new Set([
  "upsert_deadline",
  "complete_deadline",
  "delete_deadline",
  "upsert_grade_category",
  "add_grade_item",
  "write_note",
  "trigger_synthesis",
  "generate_practice",
  "propose_file_moves",
]);

export function isWriteTool(item: { name: string; isWrite?: boolean }): boolean {
  return item.isWrite ?? WRITE_TOOLS.has(item.name);
}
