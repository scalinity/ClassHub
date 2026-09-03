import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, ChevronRight, Pencil, Trash2 } from "lucide-react";

import {
  approveDeadlineProposals,
  DEADLINE_KINDS,
  deadlineSourceBadge,
  deleteDeadline,
  getDeadlineProposals,
  listDeadlines,
  resolveDeadlineProposal,
  runSyllabusScan,
  saveDeadline,
  setDeadlineStatus,
  type Deadline,
  type DeadlineKind,
  type DeadlineProposal,
} from "@/lib/deadlines";
import { useJobs } from "@/lib/jobs";
import type { TreeNode } from "@/lib/materials";
import { daysUntil, dueDayLabel, formatDueDate } from "@/lib/schedule";
import { monoAction, inputBase } from "@/lib/styles";

function collectFiles(
  nodes: readonly TreeNode[],
  out: TreeNode[] = [],
): TreeNode[] {
  for (const node of nodes) {
    if (node.dir) collectFiles(node.children, out);
    else out.push(node);
  }
  return out;
}

/**
 * SPEC §11 — the per-class deadline list with CRUD, and the syllabus-scan
 * flow: pick a file (or the whole folder) → read-only job proposes dated
 * items → confirm cards → approval inserts with source='syllabus'. Nothing
 * becomes a deadline without a click.
 */
export function DeadlinesSection({
  classId,
  tree,
}: {
  classId: number;
  /** undefined while the classTree query is in flight (the scan picker waits). */
  tree: readonly TreeNode[] | undefined;
}) {
  const { data: all } = useQuery({
    queryKey: ["deadlines"],
    queryFn: listDeadlines,
  });
  const { data: proposals } = useQuery({
    queryKey: ["deadlineProposals", classId],
    queryFn: () => getDeadlineProposals(classId),
    placeholderData: (prev) => prev,
  });
  const { jobs } = useJobs();

  // "new" opens a blank form; a Deadline opens it prefilled.
  const [editing, setEditing] = useState<Deadline | "new" | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [showDone, setShowDone] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  // Proposal ids resolved this session — their cards leave the queue the
  // moment the backend confirms, without waiting on the refetch.
  const [resolvedIds, setResolvedIds] = useState<ReadonlySet<number>>(
    new Set(),
  );
  const [addingAll, setAddingAll] = useState(false);
  // Held while the scan command is in flight: a double-click in the picker
  // would otherwise race the backend's already-queued guard and enqueue two
  // identical scans.
  const [scanStarting, setScanStarting] = useState(false);

  const deadlines = (all ?? []).filter((d) => d.classId === classId);
  const open = deadlines.filter((d) => d.status === "open");
  const done = deadlines.filter((d) => d.status === "done");
  const cards = (proposals ?? []).filter((p) => !resolvedIds.has(p.id));
  // A date already gone is not what ADD ALL is for: it may be a real deadline
  // entered late, or a scan misreading last year's syllabus, and only its own
  // card can tell. Each stays addable one at a time.
  const pastCards = cards.filter((p) => daysUntil(p.dueAt) < 0);
  const addable = cards.filter((p) => daysUntil(p.dueAt) >= 0);

  const scanJobs = jobs.filter(
    (j) => j.kind === "syllabus_scan" && j.classId === classId,
  );
  const activeScan =
    scanJobs.find((j) => j.status === "running" || j.status === "queued") ??
    null;
  // jobs arrive newest-first; a failure matters until the next scan.
  const lastFailed =
    !activeScan && scanJobs[0]?.status === "failed" ? scanJobs[0] : null;

  const startScan = (relPath: string | null) => {
    if (scanStarting) return;
    setScanStarting(true);
    setActionError(null);
    runSyllabusScan(classId, relPath)
      .then(() => {
        setScanStarting(false);
        setPickerOpen(false);
      })
      .catch((e) => {
        setActionError(String(e));
        setScanStarting(false);
        setPickerOpen(false);
      });
  };

  const markResolved = (id: number) =>
    setResolvedIds((prev) => new Set(prev).add(id));

  // One backend call for the batch: a rejected card (e.g. its deadline was
  // added by hand after the scan) costs itself, never the rest, and its
  // reason is surfaced while the card stays in the queue.
  const addAll = () => {
    setAddingAll(true);
    setActionError(null);
    approveDeadlineProposals(addable.map((card) => card.id))
      .then((outcome) => {
        setResolvedIds((prev) => {
          const next = new Set(prev);
          for (const id of outcome.approved) next.add(id);
          return next;
        });
        if (outcome.skipped.length > 0) {
          setActionError(
            `Not added — ${outcome.skipped.join(" · ")}`,
          );
        }
        setAddingAll(false);
      })
      .catch((e) => {
        setActionError(String(e));
        setAddingAll(false);
      });
  };

  return (
    <section className="mt-12" aria-label="Deadlines">
      <div className="flex items-baseline justify-between border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          DEADLINES
        </h2>
        <div className="flex items-center gap-4">
          {activeScan ? (
            <span className="flex items-center gap-1.5 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
              <span
                aria-hidden
                className="size-1.5 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
              />
              {activeScan.status === "running"
                ? "SCANNING FOR DEADLINES…"
                : "SCAN QUEUED"}
            </span>
          ) : (
            <button
              type="button"
              onClick={() => setPickerOpen((prev) => !prev)}
              aria-expanded={pickerOpen}
              className={`${monoAction} ${
                pickerOpen
                  ? "bg-muted text-foreground"
                  : "text-muted-foreground hover:bg-muted hover:text-foreground"
              }`}
            >
              SCAN SYLLABUS
            </button>
          )}
          <button
            type="button"
            onClick={() => setEditing("new")}
            className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
          >
            ADD DEADLINE
          </button>
          {open.length > 0 && (
            <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
              {open.length === 1 ? "1 OPEN" : `${open.length} OPEN`}
            </span>
          )}
        </div>
      </div>

      {actionError && (
        <p className="mt-3 font-mono text-[11px] text-destructive">
          ✕ {actionError}
        </p>
      )}
      {lastFailed && (
        <p className="mt-3 flex items-center gap-2 font-mono text-[11px] text-destructive">
          <span className="min-w-0 truncate">
            ✕ SCAN FAILED — {lastFailed.error ?? "unknown error"}
          </span>
          <button
            type="button"
            onClick={() => startScan(lastFailed.scope)}
            className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
          >
            RETRY
          </button>
        </p>
      )}

      {pickerOpen && !activeScan && (
        <div className="mt-3 max-h-52 overflow-y-auto rounded-md border p-1">
          <p className="px-2 py-1.5 text-[12px] text-muted-foreground">
            Pick the file to scan for dated items — or scan everything.
          </p>
          <button
            type="button"
            onClick={() => startScan(null)}
            disabled={scanStarting}
            className="block w-full cursor-pointer rounded px-2 py-1 text-left font-mono text-[11px] font-medium text-(--accent) transition-colors hover:bg-(--accent)/12 focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-60"
          >
            WHOLE CLASS FOLDER
          </button>
          {tree === undefined ? (
            <p className="px-2 py-1.5 text-[12px] text-muted-foreground">
              Files are still loading…
            </p>
          ) : (
            collectFiles(tree).map((file) => (
              <button
                key={file.relPath}
                type="button"
                onClick={() => startScan(file.relPath)}
                disabled={scanStarting}
                className="block w-full cursor-pointer truncate rounded px-2 py-1 text-left font-mono text-[11px] text-muted-foreground transition-colors hover:bg-(--accent)/12 hover:text-(--accent) focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-60"
              >
                {file.relPath}
              </button>
            ))
          )}
        </div>
      )}

      {cards.length > 0 && (
        <div className="mt-4 space-y-2">
          <div className="flex items-center justify-between">
            {/* Two readers land in this queue now, and each card says which
                one proposed it. The line above them carries the promise that
                covers all of them. */}
            <p className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground">
              NOTHING IS ADDED UNTIL YOU CONFIRM
              {pastCards.length > 0 && (
                <span className="text-muted-foreground/60">
                  {" · ADD ALL SKIPS "}
                  {pastCards.length === 1
                    ? "1 PAST DATE"
                    : `${pastCards.length} PAST DATES`}
                </span>
              )}
            </p>
            {addable.length > 0 && (
              <button
                type="button"
                onClick={addAll}
                disabled={addingAll}
                className={`${monoAction} text-(--accent) hover:bg-(--accent)/12 disabled:pointer-events-none disabled:opacity-60`}
              >
                {addingAll ? "ADDING…" : `ADD ALL ${addable.length}`}
              </button>
            )}
          </div>
          {cards.map((proposal) => (
            <ProposalCard
              key={proposal.id}
              proposal={proposal}
              disabled={addingAll}
              onResolved={markResolved}
            />
          ))}
        </div>
      )}

      {editing !== null && (
        // Keyed by target: the form seeds its fields in useState initializers,
        // so switching edit targets must remount it — a reused instance would
        // keep the previous row's values while saving under the new row's id.
        <DeadlineForm
          key={editing === "new" ? "new" : editing.id}
          classId={classId}
          deadline={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
        />
      )}

      <div className="mt-3 space-y-0.5">
        {open.length === 0 && cards.length === 0 && editing === null && (
          <p className="py-2 text-[13px] text-muted-foreground">
            No open deadlines — add one, or scan the syllabus for dates.
          </p>
        )}
        {open.map((deadline) => (
          <DeadlineRow
            key={deadline.id}
            deadline={deadline}
            onEdit={() => setEditing(deadline)}
            onError={setActionError}
          />
        ))}
      </div>

      {done.length > 0 && (
        <div className="mt-2">
          <button
            type="button"
            onClick={() => setShowDone((prev) => !prev)}
            aria-expanded={showDone}
            className="flex cursor-pointer items-center gap-1 rounded px-2 py-1 font-mono text-[10px] tracking-[0.14em] text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
          >
            <ChevronRight
              size={11}
              aria-hidden
              className={
                "transition-transform duration-150 " +
                (showDone ? "rotate-90" : "")
              }
            />
            {done.length} DONE
          </button>
          {showDone && (
            <div className="mt-1 space-y-0.5">
              {done.map((deadline) => (
                <DeadlineRow
                  key={deadline.id}
                  deadline={deadline}
                  onEdit={() => setEditing(deadline)}
                  onError={setActionError}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

function DeadlineRow({
  deadline,
  onEdit,
  onError,
}: {
  deadline: Deadline;
  onEdit: () => void;
  onError: (message: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const isDone = deadline.status === "done";
  const overdue = !isDone && daysUntil(deadline.dueAt) < 0;

  const run = (action: Promise<void>) => {
    setBusy(true);
    action
      .then(() => setBusy(false))
      .catch((e) => {
        onError(String(e));
        setBusy(false);
      });
  };

  return (
    <div
      className={
        "group flex h-9 items-center gap-2.5 rounded-md px-2 transition-colors hover:bg-muted/60 " +
        (isDone ? "opacity-60" : "")
      }
    >
      <button
        type="button"
        title={isDone ? "Reopen this deadline" : "Mark done"}
        aria-label={
          isDone ? `Reopen ${deadline.title}` : `Mark ${deadline.title} done`
        }
        disabled={busy}
        onClick={() => run(setDeadlineStatus(deadline.id, !isDone))}
        className={
          "flex size-[15px] shrink-0 cursor-pointer items-center justify-center rounded-full border transition-colors focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none " +
          (isDone
            ? "border-(--accent) bg-(--accent) text-white"
            : "border-muted-foreground/40 hover:border-(--accent)")
        }
      >
        {isDone && <Check size={10} strokeWidth={3} aria-hidden />}
      </button>
      <span
        className={
          "shrink-0 font-mono text-[11px] font-medium " +
          (isDone
            ? "text-muted-foreground"
            : overdue
              ? "text-destructive"
              : "text-(--accent)")
        }
      >
        {isDone ? formatDueDate(deadline.dueAt) : dueDayLabel(deadline.dueAt)}
      </span>
      <span
        title={deadline.notes ?? undefined}
        className={
          "min-w-0 flex-1 truncate text-[13px] " +
          (isDone ? "text-muted-foreground line-through" : "")
        }
      >
        {deadline.title}
      </span>
      {deadline.kind !== "other" && (
        <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-[9px] tracking-[0.12em] text-muted-foreground">
          {deadline.kind.toUpperCase()}
        </span>
      )}
      {deadlineSourceBadge(deadline.source) && (
        <span
          title={deadlineSourceBadge(deadline.source)?.title}
          className="shrink-0 font-mono text-[9px] tracking-[0.12em] text-muted-foreground/60"
        >
          {deadlineSourceBadge(deadline.source)?.label}
        </span>
      )}
      <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
        <button
          type="button"
          title="Edit deadline"
          aria-label={`Edit ${deadline.title}`}
          disabled={busy}
          onClick={onEdit}
          className="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <Pencil size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Delete deadline (kept in the audit log)"
          aria-label={`Delete ${deadline.title}`}
          disabled={busy}
          onClick={() => run(deleteDeadline(deadline.id))}
          className="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-destructive focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <Trash2 size={12} aria-hidden />
        </button>
      </span>
    </div>
  );
}

function DeadlineForm({
  classId,
  deadline,
  onClose,
}: {
  classId: number;
  /** null creates; a row prefills and amends. */
  deadline: Deadline | null;
  onClose: () => void;
}) {
  const [title, setTitle] = useState(deadline?.title ?? "");
  const [date, setDate] = useState(deadline?.dueAt.split("T")[0] ?? "");
  const [time, setTime] = useState(
    deadline?.dueAt.includes("T") ? deadline.dueAt.split("T")[1].slice(0, 5) : "",
  );
  const [kind, setKind] = useState<DeadlineKind>(deadline?.kind ?? "assignment");
  const [notes, setNotes] = useState(deadline?.notes ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSave = title.trim() !== "" && date !== "";
  const save = () => {
    setBusy(true);
    setError(null);
    saveDeadline({
      classId,
      id: deadline?.id,
      title: title.trim(),
      kind,
      dueAt: time ? `${date}T${time}` : date,
      notes: notes.trim() || undefined,
    })
      .then(onClose)
      .catch((e) => {
        setError(String(e));
        setBusy(false);
      });
  };

  return (
    <div className="mt-4 rounded-lg border bg-card px-4 py-3">
      <p className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground">
        {deadline ? "EDIT DEADLINE" : "NEW DEADLINE"}
      </p>
      <div className="mt-2.5 flex flex-wrap gap-2">
        <input
          type="text"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="What is due"
          aria-label="Deadline title"
          autoFocus
          className={`${inputBase} min-w-40 flex-1`}
        />
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value as DeadlineKind)}
          aria-label="Deadline kind"
          className={`${inputBase} cursor-pointer`}
        >
          {DEADLINE_KINDS.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
      </div>
      <div className="mt-2 flex flex-wrap gap-2">
        <input
          type="date"
          value={date}
          onChange={(e) => setDate(e.target.value)}
          aria-label="Due date"
          className={`${inputBase} font-mono text-[12px]`}
        />
        <input
          type="time"
          value={time}
          onChange={(e) => setTime(e.target.value)}
          aria-label="Due time (optional)"
          className={`${inputBase} font-mono text-[12px]`}
        />
        <input
          type="text"
          value={notes}
          onChange={(e) => setNotes(e.target.value)}
          placeholder="Notes (optional)"
          aria-label="Deadline notes"
          className={`${inputBase} min-w-40 flex-1`}
        />
      </div>
      <div className="mt-3 flex items-center gap-1">
        <button
          type="button"
          onClick={save}
          disabled={busy || !canSave}
          className={`${monoAction} bg-(--accent)/12 text-(--accent) hover:bg-(--accent)/20 disabled:pointer-events-none disabled:opacity-60`}
        >
          {busy ? "SAVING…" : "SAVE DEADLINE"}
        </button>
        <button
          type="button"
          onClick={onClose}
          disabled={busy}
          className={`${monoAction} text-muted-foreground hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-60`}
        >
          CANCEL
        </button>
      </div>
      {error && (
        <p className="mt-2 font-mono text-[11px] text-destructive">✕ {error}</p>
      )}
    </div>
  );
}

/** One proposed deadline from the syllabus scan, awaiting a decision. */
function ProposalCard({
  proposal,
  disabled,
  onResolved,
}: {
  proposal: DeadlineProposal;
  disabled: boolean;
  onResolved: (id: number) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Before today: shown in the muted register with a PAST tag rather than as
  // OVERDUE — nothing is overdue until it is a deadline.
  const past = daysUntil(proposal.dueAt) < 0;

  const resolve = (approve: boolean) => {
    setBusy(true);
    setError(null);
    resolveDeadlineProposal(proposal.id, approve)
      .then(() => onResolved(proposal.id))
      .catch((e) => {
        setError(String(e));
        setBusy(false);
      });
  };

  return (
    <div className="rounded-lg border bg-card px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <p className="min-w-0 truncate text-[13px] font-medium">
          {proposal.title}
        </p>
        <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-[9px] tracking-[0.12em] text-muted-foreground">
          {proposal.kind.toUpperCase()}
        </span>
      </div>
      <p
        className={
          "mt-1 flex items-baseline gap-2 font-mono text-[11px] font-medium " +
          (past ? "text-muted-foreground" : "text-(--accent)")
        }
      >
        {past ? formatDueDate(proposal.dueAt) : dueDayLabel(proposal.dueAt)}
        {past && (
          <span
            title="This date has already passed — add it only if it is a real deadline entered late"
            className="rounded border px-1 py-px text-[9px] font-normal tracking-[0.12em] text-muted-foreground"
          >
            PAST
          </span>
        )}
        <span
          title={deadlineSourceBadge(proposal.source)?.title}
          className="font-normal tracking-[0.12em] text-[9px] text-muted-foreground/60"
        >
          {deadlineSourceBadge(proposal.source)?.label}
        </span>
      </p>
      {proposal.notes && (
        <p className="mt-1.5 text-[12.5px] leading-relaxed text-muted-foreground">
          {proposal.notes}
        </p>
      )}
      <div className="mt-2.5 flex items-center gap-1">
        <button
          type="button"
          onClick={() => resolve(true)}
          disabled={busy || disabled}
          className={`${monoAction} bg-(--accent)/12 text-(--accent) hover:bg-(--accent)/20 disabled:pointer-events-none disabled:opacity-60`}
        >
          {busy ? "WORKING…" : "ADD DEADLINE"}
        </button>
        <button
          type="button"
          onClick={() => resolve(false)}
          disabled={busy || disabled}
          className={`${monoAction} text-muted-foreground hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-60`}
        >
          SKIP
        </button>
      </div>
      {error && (
        <p className="mt-2 font-mono text-[11px] text-destructive">✕ {error}</p>
      )}
    </div>
  );
}
