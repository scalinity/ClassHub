import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";

import { docShell, renderMarkdown } from "@/lib/document";
import { readClassFile, revealInFinder, saveNote } from "@/lib/materials";
import { queryClient } from "@/lib/query";
import { dragWindow } from "@/lib/window";
import { headerAction } from "@/lib/styles";

export interface EditedNote {
  /** null starts a blank note; a title opens `Notes/<title>.md`. */
  title: string | null;
  relPath: string | null;
}

/**
 * SPEC §11 — the markdown notes editor: textarea beside a live preview
 * rendered through the same document shell every note is read with. Closing
 * saves anything unsaved (an overwrite parks the previous version in the
 * audit log — recoverability in place of a confirmation prompt).
 */
export function NoteEditor({
  classId,
  note,
  onClose,
}: {
  classId: number;
  note: EditedNote;
  onClose: () => void;
}) {
  const existing = note.relPath !== null;
  const { data, error } = useQuery({
    queryKey: ["classFile", classId, note.relPath],
    queryFn: () => readClassFile(classId, note.relPath ?? ""),
    enabled: existing,
    retry: false,
  });

  if (existing && data === undefined) {
    return (
      <Chrome onClose={onClose} title={note.title ?? ""} status={null}>
        <p className="py-16 text-center font-mono text-xs text-muted-foreground">
          {error ? `COULD NOT LOAD NOTE — ${String(error)}` : "LOADING…"}
        </p>
      </Chrome>
    );
  }

  return (
    <EditorBody
      // Keyed by target so the editor's state seeds once per note.
      key={note.relPath ?? "new"}
      classId={classId}
      initialTitle={note.title}
      initialRelPath={note.relPath}
      initialContent={data ?? ""}
      onClose={onClose}
    />
  );
}

function EditorBody({
  classId,
  initialTitle,
  initialRelPath,
  initialContent,
  onClose,
}: {
  classId: number;
  initialTitle: string | null;
  initialRelPath: string | null;
  initialContent: string;
  onClose: () => void;
}) {
  const [title, setTitle] = useState(initialTitle ?? "");
  // A first save fixes the title — the file name comes from it.
  const [titleFixed, setTitleFixed] = useState(initialTitle !== null);
  // The canonical file this editor writes: the backend's sanitizer names the
  // file, so the raw title must never be used to address it (a `Week 3:
  // recap` title lives at `Notes/Week 3- recap.md`).
  const [relPath, setRelPath] = useState(initialRelPath);
  const [content, setContent] = useState(initialContent);
  const [savedContent, setSavedContent] = useState(initialContent);
  const [savedLabel, setSavedLabel] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const dirty = content !== savedContent;
  const savable = title.trim() !== "" && content.trim() !== "";
  // Parse only when the content changes — every other render (title
  // keystrokes, busy flips) reuses the built document.
  const previewHtml = useMemo(() => docShell(renderMarkdown(content)), [content]);

  const save = (thenClose: boolean) => {
    if (busy || !dirty || !savable) return;
    setBusy(true);
    setSaveError(null);
    saveNote(classId, title.trim(), content, relPath ?? undefined)
      .then(({ relPath: savedPath }) => {
        // The read cache must match the disk immediately: reopening seeds the
        // editor from this key, and a background refetch would land too late.
        queryClient.setQueryData(
          ["classFile", classId, savedPath],
          content.endsWith("\n") ? content : `${content}\n`,
        );
        setRelPath(savedPath);
        setSavedContent(content);
        setTitleFixed(true);
        setSavedLabel(
          new Date()
            .toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })
            .toUpperCase(),
        );
        setBusy(false);
        if (thenClose) onClose();
      })
      .catch((e) => {
        setSaveError(String(e));
        setBusy(false);
      });
  };

  // Closing keeps work: unsaved content is saved on the way out (the audit
  // log holds the replaced version). Untitled text is the one thing a close
  // could silently lose, so that close is held until it has a title — or the
  // text is cleared to discard it.
  const close = () => {
    if (busy) return;
    if (!dirty) {
      onClose();
      return;
    }
    if (!savable) {
      if (content.trim() === "") {
        onClose();
        return;
      }
      setSaveError("Give the note a title to keep it — or clear the text to discard.");
      return;
    }
    save(true);
  };

  const status = busy
    ? { label: "SAVING…", tone: "muted" as const }
    : saveError
      ? { label: `✕ ${saveError}`, tone: "error" as const }
      : dirty
        ? {
            label: savable ? "EDITED — SAVES ON CLOSE" : "NEEDS A TITLE",
            tone: "amber" as const,
          }
        : savedLabel
          ? { label: `SAVED ${savedLabel}`, tone: "muted" as const }
          : null;

  return (
    <Chrome
      onClose={close}
      status={status}
      title={
        titleFixed ? (
          title
        ) : (
          <input
            type="text"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="Note title…"
            aria-label="Note title"
            autoFocus
            spellCheck={false}
            className="w-full min-w-0 bg-transparent text-[12px] outline-none placeholder:text-muted-foreground/60"
          />
        )
      }
      actions={
        <>
          <button
            type="button"
            onClick={() => save(false)}
            disabled={busy || !dirty || !savable}
            className={`${headerAction} disabled:pointer-events-none disabled:opacity-40`}
          >
            SAVE
          </button>
          {relPath !== null && (
            <button
              type="button"
              onClick={() =>
                void revealInFinder(classId, relPath).catch((e) =>
                  setSaveError(String(e)),
                )
              }
              className={headerAction}
            >
              SHOW IN FINDER
            </button>
          )}
        </>
      }
    >
      <div className="grid h-full grid-cols-2">
        <textarea
          value={content}
          onChange={(e) => setContent(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
              e.preventDefault();
              save(false);
            }
          }}
          placeholder={"Write in Markdown — # headings, **bold**, `code`, lists…"}
          aria-label="Note content in Markdown"
          autoFocus={titleFixed}
          spellCheck={false}
          className="h-full w-full resize-none border-r bg-transparent px-6 py-5 font-mono text-[12.5px] leading-relaxed outline-none placeholder:text-muted-foreground/50"
        />
        {/* allow-same-origin (still no scripts) lets the in-place rewrite
            reach the document — the FileViewer live-mode pattern. */}
        <iframe
          sandbox="allow-same-origin"
          title="Note preview"
          ref={(el) => syncPreview(el, previewHtml)}
          className="block h-full w-full border-0"
        />
      </div>
    </Chrome>
  );
}

/** Last document written per preview frame, to skip no-op rewrites. */
const written = new WeakMap<HTMLIFrameElement, string>();

/** Rewrites the frame in place (doc.write keeps it live), preserving scroll. */
function syncPreview(el: HTMLIFrameElement | null, html: string) {
  if (!el || written.get(el) === html) return;
  const doc = el.contentDocument;
  if (!doc) return;
  const top = doc.scrollingElement?.scrollTop ?? 0;
  doc.open();
  doc.write(html);
  doc.close();
  written.set(el, html);
  if (doc.scrollingElement) doc.scrollingElement.scrollTop = top;
}

function Chrome({
  title,
  status,
  actions,
  onClose,
  children,
}: {
  title: React.ReactNode;
  status: { label: string; tone: "muted" | "amber" | "error" } | null;
  actions?: React.ReactNode;
  onClose: () => void;
  children: React.ReactNode;
}) {
  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Note editor"
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose();
      }}
      className="fixed inset-0 z-40 flex flex-col bg-background animate-in fade-in duration-150 motion-reduce:animate-none"
    >
      <header
        onMouseDown={dragWindow}
        className="flex h-12 shrink-0 items-center gap-2.5 border-b bg-card pl-24 pr-3"
      >
        <p className="pointer-events-none shrink-0 font-mono text-[11px] tracking-[0.18em] text-(--accent)">
          NOTE
        </p>
        <div className="min-w-0 flex-1 truncate text-[12px] text-muted-foreground">
          {title}
        </div>
        {status && (
          <span
            className={
              "pointer-events-none min-w-0 shrink-[2] truncate font-mono text-[10px] tracking-[0.14em] " +
              (status.tone === "error"
                ? "text-destructive"
                : status.tone === "amber"
                  ? "text-class-amber"
                  : "text-muted-foreground/70")
            }
          >
            {status.label}
          </span>
        )}
        {actions}
        <button
          type="button"
          aria-label="Close note editor"
          onClick={onClose}
          className="shrink-0 cursor-pointer rounded p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <X size={14} aria-hidden />
        </button>
      </header>
      <div className="min-h-0 flex-1">{children}</div>
    </div>
  );
}
