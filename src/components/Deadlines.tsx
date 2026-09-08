import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, ChevronRight, Pencil, Trash2 } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import { canvasSyllabus } from "@/lib/canvas";
import {
  approveDeadlineProposals,
  DEADLINE_KINDS,
  deadlineSourceBadge,
  deleteDeadline,
  dismissDeadlineProposals,
  getDeadlineProposals,
  listDeadlines,
  resolveDeadlineProposal,
  runSyllabusScan,
  saveDeadline,
  setDeadlineStatus,
  type Deadline,
  type DeadlineKind,
  type DeadlineProposal,
  type DeadlineSeries,
} from "@/lib/deadlines";
import { useJobs } from "@/lib/jobs";
import type { TreeNode } from "@/lib/materials";
import { daysUntil, dueDayLabel, formatDueDate, isOverdue } from "@/lib/schedule";
import {
  buttonFilled,
  buttonIcon,
  buttonText,
  buttonTextMuted,
  checkCircle,
  checkCircleDone,
  chipMuted,
  decisionCard,
  errorLine,
  input,
  pulseDot,
  readingText,
  row,
  statusLine,
} from "@/lib/styles";

/** One item of the scan picker: a text action laid out as a menu row. */
const pickerItem =
  "block w-full cursor-pointer rounded-sm px-2 py-1 text-left text-body font-medium text-(--accent-ink) transition-colors hover:bg-(--accent)/10 focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-60";

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
  const { data: queue } = useQuery({
    queryKey: ["deadlineProposals", classId],
    queryFn: () => getDeadlineProposals(classId),
    placeholderData: (prev) => prev,
  });
  // The Canvas syllabus page a sync mirrored, when there is one — the tree
  // never lists it, since the extract cache is hidden from the scan.
  const { data: canvasSyllabusPath } = useQuery({
    queryKey: ["canvasSyllabus", classId],
    queryFn: () => canvasSyllabus(classId),
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
  const cards = (queue?.proposals ?? []).filter((p) => !resolvedIds.has(p.id));
  // A recurring proposal reads as one card (SPEC §11), shown while none of
  // its dates has been resolved here; its members leave the singles.
  const series = (queue?.series ?? []).filter((s) =>
    s.ids.every((id) => !resolvedIds.has(id)),
  );
  const inSeries = new Set(series.flatMap((s) => s.ids));
  const singles = cards.filter((p) => !inSeries.has(p.id));
  // A date already gone is not what Add all is for: it may be a real deadline
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

  const markResolved = (ids: number[]) =>
    setResolvedIds((prev) => {
      const next = new Set(prev);
      for (const id of ids) next.add(id);
      return next;
    });

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
          setActionError(`Not added: ${outcome.skipped.join(" · ")}`);
        }
        setAddingAll(false);
      })
      .catch((e) => {
        setActionError(String(e));
        setAddingAll(false);
      });
  };

  return (
    <section id="deadlines" className="mt-14 scroll-mt-20" aria-label="Deadlines">
      <SectionHeading
        title="Deadlines"
        count={
          open.length === 0
            ? undefined
            : open.length === 1
              ? "1 open"
              : `${open.length} open`
        }
        actions={
          <>
            {activeScan ? (
              <span className={`px-2 ${statusLine}`}>
                <span aria-hidden className={pulseDot} />
                {activeScan.status === "running"
                  ? "Scanning for deadlines…"
                  : "Scan queued"}
              </span>
            ) : (
              <button
                type="button"
                onClick={() => setPickerOpen((prev) => !prev)}
                aria-expanded={pickerOpen}
                className={`${buttonTextMuted} ${pickerOpen ? "bg-muted text-foreground" : ""}`}
              >
                Scan the syllabus
              </button>
            )}
            <button
              type="button"
              onClick={() => setEditing("new")}
              className={buttonText}
            >
              Add deadline
            </button>
          </>
        }
      >
        {actionError && <p className={errorLine}>{actionError}</p>}
        {lastFailed && (
          <p className={`${errorLine} flex items-center gap-2`}>
            <span className="min-w-0 truncate">
              The scan failed: {lastFailed.error ?? "unknown error"}
            </span>
            <button
              type="button"
              onClick={() => startScan(lastFailed.scope)}
              className={buttonText}
            >
              Retry
            </button>
          </p>
        )}
      </SectionHeading>

      {pickerOpen && !activeScan && (
        <div className="mt-3 max-h-52 overflow-y-auto rounded-md p-1 ring-1 ring-border">
          <p className="px-2 py-1.5 text-body text-muted-foreground">
            Pick the file to scan for dated items — or scan everything.
          </p>
          <button
            type="button"
            onClick={() => startScan(null)}
            disabled={scanStarting}
            className={pickerItem}
          >
            Whole class folder
          </button>
          {canvasSyllabusPath && (
            <button
              type="button"
              title={canvasSyllabusPath}
              onClick={() => startScan(canvasSyllabusPath)}
              disabled={scanStarting}
              className={pickerItem}
            >
              Canvas syllabus page
            </button>
          )}
          {tree === undefined ? (
            <p className="px-2 py-1.5 text-body text-muted-foreground">
              Files are still loading…
            </p>
          ) : (
            collectFiles(tree).map((file) => (
              <button
                key={file.relPath}
                type="button"
                onClick={() => startScan(file.relPath)}
                disabled={scanStarting}
                className="block w-full cursor-pointer truncate rounded-sm px-2 py-1 text-left font-mono text-code text-muted-foreground transition-colors hover:bg-(--accent)/10 hover:text-(--accent-ink) focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-60"
              >
                {file.relPath}
              </button>
            ))
          )}
        </div>
      )}

      {cards.length > 0 && (
        <div className="mt-4 space-y-3">
          <div className="flex items-center justify-between gap-4">
            {/* Two readers land in this queue now, and each card says which
                one proposed it. The line above them carries the promise that
                covers all of them. */}
            <p className="text-meta text-muted-foreground">
              Nothing is added until you confirm
              {pastCards.length > 0 && (
                <span className="text-muted-foreground/70">
                  {" · Add all skips "}
                  {pastCards.length === 1
                    ? "1 past date"
                    : `${pastCards.length} past dates`}
                </span>
              )}
            </p>
            {addable.length > 0 && (
              <button
                type="button"
                onClick={addAll}
                disabled={addingAll}
                className={buttonText}
              >
                {addingAll ? "Adding…" : `Add all ${addable.length}`}
              </button>
            )}
          </div>
          {series.map((group) => (
            <SeriesCard
              key={group.ids.join("-")}
              series={group}
              disabled={addingAll}
              onResolved={markResolved}
            />
          ))}
          {singles.map((proposal) => (
            <ProposalCard
              key={proposal.id}
              proposal={proposal}
              disabled={addingAll}
              onResolved={(id) => markResolved([id])}
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

      <div className="mt-3">
        {open.length === 0 && cards.length === 0 && editing === null && (
          <p className={`py-2 ${readingText} text-muted-foreground`}>
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
            className={`${buttonTextMuted} -ml-2`}
          >
            <ChevronRight
              size={12}
              aria-hidden
              className={
                "transition-transform duration-150 " +
                (showDone ? "rotate-90" : "")
              }
            />
            {done.length} done
          </button>
          {showDone && (
            <div className="mt-1">
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
  const overdue = !isDone && isOverdue(deadline.dueAt);
  const badge = deadlineSourceBadge(deadline.source, deadline.canvasAssignmentId);

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
    <div className={`${row} ${isDone ? "opacity-60" : ""}`}>
      <button
        type="button"
        title={isDone ? "Reopen this deadline" : "Mark done"}
        aria-label={
          isDone ? `Reopen ${deadline.title}` : `Mark ${deadline.title} done`
        }
        disabled={busy}
        onClick={() => run(setDeadlineStatus(deadline.id, !isDone))}
        className={
          `${checkCircle} cursor-pointer focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none ` +
          (isDone ? checkCircleDone : "border-muted-foreground/40 hover:border-(--accent)")
        }
      >
        {isDone && <Check size={10} strokeWidth={3} aria-hidden />}
      </button>
      <span
        title={deadline.notes ?? undefined}
        className={
          "min-w-0 flex-1 truncate text-title " +
          (isDone ? "text-muted-foreground line-through" : "")
        }
      >
        {deadline.title}
      </span>
      {deadline.kind !== "other" && (
        <span className={chipMuted}>{deadline.kind}</span>
      )}
      {badge && (
        <span title={badge.title} className="shrink-0 text-fine text-muted-foreground">
          {badge.label}
        </span>
      )}
      <span className="flex shrink-0 items-center gap-0.5">
        <button
          type="button"
          title="Edit deadline"
          aria-label={`Edit ${deadline.title}`}
          disabled={busy}
          onClick={onEdit}
          className={buttonIcon}
        >
          <Pencil size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Delete deadline (kept in the audit log)"
          aria-label={`Delete ${deadline.title}`}
          disabled={busy}
          onClick={() => run(deleteDeadline(deadline.id))}
          className={`${buttonIcon} hover:text-destructive`}
        >
          <Trash2 size={12} aria-hidden />
        </button>
      </span>
      <span
        className={
          "shrink-0 text-meta tabular-nums " +
          (isDone
            ? "text-muted-foreground"
            : overdue
              ? "font-medium text-destructive"
              : "font-medium text-(--accent-ink)")
        }
      >
        {isDone ? formatDueDate(deadline.dueAt) : dueDayLabel(deadline.dueAt)}
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
    <div className={`${decisionCard} mt-4`}>
      <p className="text-[15px] font-semibold">
        {deadline ? "Edit deadline" : "New deadline"}
      </p>
      <div className="mt-3 flex flex-wrap gap-2">
        <input
          type="text"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="What is due"
          aria-label="Deadline title"
          autoFocus
          className={`${input} min-w-40 flex-1`}
        />
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value as DeadlineKind)}
          aria-label="Deadline kind"
          className={`${input} cursor-pointer`}
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
          className={`${input} tabular-nums`}
        />
        <input
          type="time"
          value={time}
          onChange={(e) => setTime(e.target.value)}
          aria-label="Due time (optional)"
          className={`${input} tabular-nums`}
        />
        <input
          type="text"
          value={notes}
          onChange={(e) => setNotes(e.target.value)}
          placeholder="Notes (optional)"
          aria-label="Deadline notes"
          className={`${input} min-w-40 flex-1`}
        />
      </div>
      <div className="mt-3 flex items-center gap-1">
        <button
          type="button"
          onClick={save}
          disabled={busy || !canSave}
          className={buttonFilled}
        >
          {busy ? "Saving…" : "Save deadline"}
        </button>
        <button
          type="button"
          onClick={onClose}
          disabled={busy}
          className={buttonTextMuted}
        >
          Cancel
        </button>
      </div>
      {error && <p className={errorLine}>{error}</p>}
    </div>
  );
}

/**
 * A recurring proposal as one card (SPEC §11): `Live coding session · 12
 * dates`, the weekday and the span, with `Add the series` — every date under
 * one batch and one Undo — and `Skip`, which dismisses them all.
 */
function SeriesCard({
  series,
  disabled,
  onResolved,
}: {
  series: DeadlineSeries;
  disabled: boolean;
  onResolved: (ids: number[]) => void;
}) {
  const [busy, setBusy] = useState<"add" | "skip" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const badge = deadlineSourceBadge(series.source);
  const count = series.ids.length;

  const add = () => {
    setBusy("add");
    setError(null);
    approveDeadlineProposals(series.ids)
      .then((outcome) => {
        onResolved(outcome.approved);
        if (outcome.skipped.length > 0) {
          setError(`Not added: ${outcome.skipped.join(" · ")}`);
        }
        setBusy(null);
      })
      .catch((e) => {
        setError(String(e));
        setBusy(null);
      });
  };
  const skip = () => {
    setBusy("skip");
    setError(null);
    dismissDeadlineProposals(series.ids)
      .then((outcome) => {
        onResolved(outcome.approved);
        if (outcome.skipped.length > 0) {
          setError(`Not skipped: ${outcome.skipped.join(" · ")}`);
        }
        setBusy(null);
      })
      .catch((e) => {
        setError(String(e));
        setBusy(null);
      });
  };

  return (
    <div className={decisionCard}>
      <div className="flex items-start justify-between gap-3">
        <p className="min-w-0 truncate text-[15px] font-semibold">
          {series.stem}
          <span className="font-normal text-muted-foreground"> · {count} dates</span>
        </p>
        <span className={chipMuted}>{series.kind}</span>
      </div>
      <p className="mt-1 flex flex-wrap items-baseline gap-2 text-meta font-medium tabular-nums text-(--accent-ink)">
        {series.weekday}s, {formatDueDate(series.firstDue)} – {formatDueDate(series.lastDue)}
        {badge && (
          <span title={badge.title} className="text-fine font-normal text-muted-foreground">
            {badge.label}
          </span>
        )}
      </p>
      <p className="mt-1.5 text-body text-muted-foreground">
        One deadline per date, {count} in all, added together and undone together.
      </p>
      <div className="mt-3 flex items-center gap-1">
        <button
          type="button"
          onClick={add}
          disabled={busy !== null || disabled}
          className={buttonFilled}
        >
          {busy === "add" ? "Adding…" : "Add the series"}
        </button>
        <button
          type="button"
          onClick={skip}
          disabled={busy !== null || disabled}
          className={buttonTextMuted}
        >
          {busy === "skip" ? "Skipping…" : "Skip"}
        </button>
      </div>
      {error && <p className={errorLine}>{error}</p>}
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
  // Before today: shown in the muted register with a "past" tag rather than
  // as overdue — nothing is overdue until it is a deadline.
  const past = daysUntil(proposal.dueAt) < 0;
  const badge = deadlineSourceBadge(proposal.source);

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
    <div className={decisionCard}>
      <div className="flex items-start justify-between gap-3">
        <p className="min-w-0 truncate text-[15px] font-semibold">
          {proposal.title}
        </p>
        <span className={chipMuted}>{proposal.kind}</span>
      </div>
      <p
        className={
          "mt-1 flex flex-wrap items-baseline gap-2 text-meta tabular-nums " +
          (past ? "text-muted-foreground" : "font-medium text-(--accent-ink)")
        }
      >
        {past ? formatDueDate(proposal.dueAt) : dueDayLabel(proposal.dueAt)}
        {past && (
          <span
            title="This date has already passed — add it only if it is a real deadline entered late"
            className={chipMuted}
          >
            past
          </span>
        )}
        {badge && (
          <span title={badge.title} className="text-fine font-normal text-muted-foreground">
            {badge.label}
          </span>
        )}
      </p>
      {proposal.notes && (
        <p className="mt-1.5 text-body text-muted-foreground">{proposal.notes}</p>
      )}
      <div className="mt-3 flex items-center gap-1">
        <button
          type="button"
          onClick={() => resolve(true)}
          disabled={busy || disabled}
          className={buttonFilled}
        >
          {busy ? "Working…" : "Add deadline"}
        </button>
        <button
          type="button"
          onClick={() => resolve(false)}
          disabled={busy || disabled}
          className={buttonTextMuted}
        >
          Skip
        </button>
      </div>
      {error && <p className={errorLine}>{error}</p>}
    </div>
  );
}
