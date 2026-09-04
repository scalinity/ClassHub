import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { File, X } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import { useJobs } from "@/lib/jobs";
import { formatSize, type TreeNode } from "@/lib/materials";
import {
  clearDropNotice,
  getSortState,
  INBOX_DIR,
  resolveProposal,
  runSortJob,
  sortByContent,
  useDragState,
  type MoveProposal,
} from "@/lib/sorter";
import {
  buttonFilled,
  buttonIcon,
  buttonText,
  buttonTextMuted,
  chip,
  chipAccent,
  chipAmber,
  chipMuted,
  decisionCard,
  errorLine,
  meta,
  pulseDot,
  rowDense,
  statusLine,
} from "@/lib/styles";

type SortState = Awaited<ReturnType<typeof getSortState>>;
type Jobs = ReturnType<typeof useJobs>["jobs"];
type DropNotice = ReturnType<typeof useDragState>["notice"];

function collectDirs(nodes: readonly TreeNode[], out: string[] = []): string[] {
  for (const node of nodes) {
    if (node.dir) {
      out.push(node.relPath);
      collectDirs(node.children, out);
    }
  }
  return out;
}

/** What the queue holds, read once for the section and once for the nav. */
function summarize(
  data: SortState | undefined,
  jobs: Jobs,
  classId: number,
  drop: DropNotice,
) {
  const sortJobs = jobs.filter(
    (j) => j.kind === "sort_proposal" && j.classId === classId,
  );
  const active =
    sortJobs.find((j) => j.status === "running" || j.status === "queued") ??
    null;
  // jobs arrive newest-first; a failure only matters while files remain unsorted.
  const lastFailed =
    !active && sortJobs[0]?.status === "failed" ? sortJobs[0] : null;
  const inbox = data?.inbox ?? [];
  const proposals = data?.proposals ?? [];
  const proposedSources = new Set(proposals.map((p) => p.sourceRelPath));
  const unproposed = inbox.filter(
    (f) => !proposedSources.has(`${INBOX_DIR}/${f.name}`) && !f.dismissed,
  );
  // Dismissed files are decisions already made: shown quietly when the
  // section is open for other reasons, never counted, never nagging.
  const dismissed = inbox.filter(
    (f) => f.dismissed && !proposedSources.has(`${INBOX_DIR}/${f.name}`),
  );
  const count = proposals.length + unproposed.length;
  const notice = drop?.classId === classId ? drop.message : null;
  return { active, lastFailed, proposals, unproposed, dismissed, count, notice };
}

/**
 * Whether the section is on screen — the workspace's nav asks the same
 * question the section answers, so both read one predicate. Dismissed files
 * are not counted, but they still keep the section on screen: Sort the inbox
 * is the documented way back out of a dismissal (sorter.rs runs manual sorts
 * over every inbox file, dismissed included), and gating on `count` alone made
 * it unreachable once everything was dismissed.
 */
export function inboxShown(
  data: SortState | undefined,
  jobs: Jobs,
  classId: number,
  drop: DropNotice,
): boolean {
  const s = summarize(data, jobs, classId, drop);
  return !(s.count === 0 && s.dismissed.length === 0 && !s.active && !s.notice);
}

/**
 * SPEC §10 steps 3–5: the drop-to-sort confirm queue. One card per pending
 * proposal — destination route, reasoning, confidence — with Approve /
 * Move to… (folder picker) / Leave in inbox; chat-filed proposals render in
 * the same queue, and a Canvas card whose destination names a week also
 * offers the week folder as a second route (SPEC §10). Inbox files nothing
 * has proposed for yet list below, with a manual Sort the inbox trigger when
 * no sort job is active.
 */
export function InboxQueue({
  classId,
  tree,
}: {
  classId: number;
  /** undefined while the classTree query is in flight — the queue renders a
   * neutral state rather than treating every folder as not-yet-existing. */
  tree: readonly TreeNode[] | undefined;
}) {
  const { data } = useQuery({
    queryKey: ["sortState", classId],
    queryFn: () => getSortState(classId),
    placeholderData: (prev) => prev,
  });
  const { jobs } = useJobs();
  const drag = useDragState();
  const [actionError, setActionError] = useState<string | null>(null);

  // Which jobs the snapshot knows: a card that started a sort holds itself
  // until the job it was handed shows up here.
  const jobIds: ReadonlySet<number> = new Set(jobs.map((j) => j.id));
  const { active, lastFailed, proposals, unproposed, dismissed, count, notice } =
    summarize(data, jobs, classId, drag.notice);

  if (!inboxShown(data, jobs, classId, drag.notice)) return null;

  const dirs = tree ? collectDirs(tree) : null;
  const dirSet: ReadonlySet<string> | null = dirs ? new Set(dirs) : null;

  const sortNow = () => {
    setActionError(null);
    runSortJob(classId).catch((e) => setActionError(String(e)));
  };

  return (
    <section
      id="inbox"
      className="mt-14 scroll-mt-20"
      aria-label="Inbox — files waiting to be sorted"
    >
      <SectionHeading
        title="Inbox"
        count={count === 0 ? undefined : count === 1 ? "1 to sort" : `${count} to sort`}
        actions={
          active ? (
            // A scoped sort is one card's: that card carries the state, and a
            // second signal in the header would announce the same run twice.
            active.scope === null ? (
              <span className={statusLine}>
                <span aria-hidden className={pulseDot} />
                {active.status === "running"
                  ? "Proposing destinations…"
                  : "Sort queued"}
              </span>
            ) : undefined
          ) : unproposed.length > 0 || dismissed.length > 0 ? (
            <button type="button" onClick={sortNow} className={buttonText}>
              Sort the inbox
            </button>
          ) : undefined
        }
      />

      {(notice ?? actionError) && (
        <p className={`${errorLine} flex items-start gap-2`}>
          <span className="min-w-0 flex-1">{notice ?? actionError}</span>
          {notice && (
            <button
              type="button"
              aria-label="Dismiss this notice"
              onClick={clearDropNotice}
              className={buttonIcon}
            >
              <X size={12} aria-hidden />
            </button>
          )}
        </p>
      )}
      {lastFailed && unproposed.length > 0 && (
        <p className={`${errorLine} flex items-center gap-2`}>
          <span className="min-w-0 truncate">
            The sort failed: {lastFailed.error ?? "unknown error"}
          </span>
          <button type="button" onClick={sortNow} className={buttonText}>
            Retry
          </button>
        </p>
      )}

      <div className="mt-4 space-y-3">
        {proposals.map((p) => (
          <ProposalCard
            key={p.id}
            proposal={p}
            dirs={dirs}
            dirSet={dirSet}
            // An explicit Sort by content carries the file as the job's scope,
            // which is how this card knows the running sort is its own. Any
            // sort for the class blocks another, so every card learns that too.
            sorting={active !== null && active.scope === p.sourceRelPath}
            sortActive={active !== null}
            jobIds={jobIds}
          />
        ))}
        {(unproposed.length > 0 || dismissed.length > 0) && (
          <div>
            {unproposed.map((f) => (
              <div key={f.name} className={rowDense}>
                <File size={14} aria-hidden className="shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1 truncate text-body">{f.name}</span>
                {active && (
                  <span className="shrink-0 text-fine text-muted-foreground">
                    awaiting a proposal
                  </span>
                )}
                <span className={`w-14 shrink-0 text-right ${meta}`}>
                  {formatSize(f.size)}
                </span>
              </div>
            ))}
            {dismissed.map((f) => (
              <div key={f.name} className={`${rowDense} opacity-60`}>
                <File size={14} aria-hidden className="shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1 truncate text-body text-muted-foreground">
                  {f.name}
                </span>
                <span
                  title="You chose to leave this file in the inbox — Sort the inbox proposes it again"
                  className="shrink-0 text-fine text-muted-foreground"
                >
                  left in the inbox
                </span>
                <span className={`w-14 shrink-0 text-right ${meta}`}>
                  {formatSize(f.size)}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

function ProposalCard({
  proposal,
  dirs,
  dirSet,
  sorting,
  sortActive,
  jobIds,
}: {
  proposal: MoveProposal;
  /** null while the tree is loading. */
  dirs: readonly string[] | null;
  dirSet: ReadonlySet<string> | null;
  /** A Sort by content job for this file is queued or running. */
  sorting: boolean;
  /** Any sort job for the class is queued or running: the backend allows one
   *  at a time, so Sort by content on another card could only be refused. */
  sortActive: boolean;
  /** Every job id the jobs snapshot currently knows. */
  jobIds: ReadonlySet<number>;
}) {
  // Busy holds until the hub-changed refetch removes the card (or an error
  // re-enables the actions) — a resolved proposal must not be re-clickable.
  const [busy, setBusy] = useState(false);
  // The sort this card started: held while the command is in flight and
  // until the job it returned reaches the jobs snapshot, after which the
  // snapshot carries the state. Not folded into `busy`: this card survives
  // the sort as the same row rewritten, so a held flag would outlive the run.
  const [sortStarting, setSortStarting] = useState(false);
  const [sortJobId, setSortJobId] = useState<number | null>(null);
  // Which approval is in flight, so each button says what it is doing and
  // the other keeps its name.
  const [filing, setFiling] = useState(false);
  const awaitingJob = sortJobId !== null && !jobIds.has(sortJobId);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const held = busy || sorting || sortStarting || awaitingJob;

  const source = proposal.sourceRelPath;
  const fileName = source.slice(source.lastIndexOf("/") + 1);
  // Move to… changes only the folder: a rename the proposal carries survives
  // the redirect, so the picker override keeps the destination's file name.
  const destName = proposal.destRelPath.slice(
    proposal.destRelPath.lastIndexOf("/") + 1,
  );
  const fromInbox = source.startsWith(`${INBOX_DIR}/`);
  const alternative = proposal.alternative;
  const fromDir = source.includes("/")
    ? source.slice(0, source.lastIndexOf("/"))
    : "";
  const pickerDirs = dirs?.filter((d) => d !== fromDir) ?? null;

  const resolve = (approve: boolean, destOverride?: string) => {
    setBusy(true);
    setError(null);
    resolveProposal(proposal.id, approve, destOverride).catch((e) => {
      setError(String(e));
      setBusy(false);
      setFiling(false);
    });
  };
  // The week folder in place of Canvas's, through the override the folder
  // picker uses: the ordinary approval, audit row and index row included.
  const fileUnderWeek = (dest: string) => {
    setFiling(true);
    resolve(true, dest);
  };

  const sortNow = () => {
    setSortStarting(true);
    setError(null);
    setPickerOpen(false);
    sortByContent(proposal.id)
      .then((jobId) => {
        setSortJobId(jobId);
        setSortStarting(false);
      })
      .catch((e) => {
        setError(String(e));
        setSortStarting(false);
      });
  };

  return (
    <div className={decisionCard}>
      <div className="flex items-start justify-between gap-3">
        <p className="min-w-0 truncate text-[15px] font-semibold">{fileName}</p>
        <ProposalChip proposal={proposal} />
      </div>

      <RouteLine proposal={proposal} dirSet={dirSet} />
      {alternative && (
        // The second destination, in the first's own register: the reader
        // sees both before choosing, and the week folder is dash-underlined
        // while it has yet to be created.
        <p className="mt-0.5 flex flex-wrap items-center gap-y-0.5 font-mono text-code">
          <span className="text-muted-foreground">or</span>
          <span aria-hidden className="px-1.5 text-(--accent-ink)">
            →
          </span>
          <DestPath
            dest={alternative.destRelPath}
            fromName={fileName}
            dirSet={dirSet}
          />
        </p>
      )}

      <p className="mt-2 text-body text-muted-foreground">{proposal.reasoning}</p>
      {alternative && (
        // The case for the second route, in the register the first's reason
        // uses and in the same order as the routes: the reader choosing
        // between two folders sees both arguments on any input, which a
        // tooltip — hover only — would not give the keyboard or touch.
        <p className="mt-1 text-body text-muted-foreground">
          {alternative.reasoning}
        </p>
      )}

      <div className="mt-3 flex flex-wrap items-center gap-1">
        <button
          type="button"
          onClick={() => resolve(true)}
          disabled={held}
          className={buttonFilled}
        >
          {busy && !filing ? "Working…" : "Approve"}
        </button>
        {alternative && (
          // The row's own action, a step earlier: the reading that would
          // offer File under Week once the file landed where Canvas put it,
          // offered on the card so one approval does the work of two.
          // Canvas's folder stays the filled default beside it.
          <button
            type="button"
            onClick={() => fileUnderWeek(alternative.destRelPath)}
            disabled={held}
            className={buttonText}
          >
            {/* The Materials row's label, word for word (FileTree.tsx,
                FilingAction): the card offers the row's own action, and the
                two must read the same. */}
            {busy && filing
              ? "Filing…"
              : `File under Week ${String(alternative.week).padStart(2, "0")}`}
          </button>
        )}
        <button
          type="button"
          onClick={() => setPickerOpen((open) => !open)}
          disabled={held}
          aria-expanded={pickerOpen}
          className={`${buttonTextMuted} ${pickerOpen ? "bg-muted text-foreground" : ""}`}
        >
          Move to…
        </button>
        {proposal.source === "canvas" &&
          // Disagreeing with the professor's folder, explicitly: the sort's
          // destination replaces Canvas's for this one file, and for no other
          // (SPEC §7.2). The result re-renders here as a sort proposal with
          // the Canvas folder still named in its reasoning.
          (sorting || sortStarting || awaitingJob ? (
            <span className={`px-2 ${statusLine}`}>
              <span aria-hidden className={pulseDot} />
              Sorting by content…
            </span>
          ) : (
            <button
              type="button"
              onClick={sortNow}
              disabled={held || sortActive}
              title={
                sortActive
                  ? "A sort job for this class is already running — one at a time"
                  : "Ask a sort job to read the file and propose a folder in place of the one Canvas keeps it in"
              }
              className={buttonTextMuted}
            >
              Sort by content
            </button>
          ))}
        <button
          type="button"
          onClick={() => resolve(false)}
          disabled={held}
          className={buttonTextMuted}
        >
          {fromInbox ? "Leave in inbox" : "Dismiss"}
        </button>
      </div>

      {pickerOpen && !held && (
        <div className="mt-2 max-h-44 overflow-y-auto rounded-md p-1 ring-1 ring-border">
          {pickerDirs === null ? (
            <p className="px-2 py-1.5 text-body text-muted-foreground">
              Folders are still loading…
            </p>
          ) : pickerDirs.length === 0 ? (
            <p className="px-2 py-1.5 text-body text-muted-foreground">
              No other folders yet — approving creates the proposed one.
            </p>
          ) : (
            pickerDirs.map((dir) => (
              <button
                key={dir}
                type="button"
                onClick={() => {
                  setPickerOpen(false);
                  resolve(true, `${dir}/${destName}`);
                }}
                className="block w-full cursor-pointer truncate rounded-sm px-2 py-1 text-left font-mono text-code text-muted-foreground transition-colors hover:bg-(--accent)/10 hover:text-(--accent-ink) focus-visible:outline-2 focus-visible:outline-(--accent)"
              >
                {dir}/
              </button>
            ))
          )}
        </div>
      )}

      {error && <p className={errorLine}>{error}</p>}
    </div>
  );
}

/**
 * The route this approval takes: source folder → destination path in mono,
 * with destination folders that don't exist yet dash-underlined in the accent
 * — the "will be created" state is visible before anything happens. While the
 * tree is loading (dirSet null) segments render plain: no folder gets called
 * new until the tree can actually say so.
 */
function RouteLine({
  proposal,
  dirSet,
}: {
  proposal: MoveProposal;
  dirSet: ReadonlySet<string> | null;
}) {
  const source = proposal.sourceRelPath;
  const fromDir = source.includes("/")
    ? source.slice(0, source.lastIndexOf("/"))
    : "class folder";
  const fromName = source.slice(source.lastIndexOf("/") + 1);

  return (
    <p className="mt-1.5 flex flex-wrap items-center gap-y-0.5 font-mono text-code">
      <span className="text-muted-foreground">{fromDir}</span>
      <span aria-hidden className="px-1.5 text-(--accent-ink)">
        →
      </span>
      <DestPath dest={proposal.destRelPath} fromName={fromName} dirSet={dirSet} />
    </p>
  );
}

/** A destination's folders, each not yet on disk marked, and its file name
 *  only where the move renames the file. */
function DestPath({
  dest,
  fromName,
  dirSet,
}: {
  dest: string;
  fromName: string;
  dirSet: ReadonlySet<string> | null;
}) {
  const lastSlash = dest.lastIndexOf("/");
  const destDir = lastSlash === -1 ? "" : dest.slice(0, lastSlash);
  const destName = dest.slice(lastSlash + 1);

  let path = "";
  const parts = (destDir === "" ? [] : destDir.split("/")).map((seg) => {
    path = path === "" ? seg : `${path}/${seg}`;
    return { seg, isNew: dirSet !== null && !dirSet.has(path) };
  });
  const hasNew = parts.some((p) => p.isNew);

  return (
    <>
      {parts.map((part, i) => (
        <span key={i} className="flex items-center">
          <span
            className={
              part.isNew
                ? "text-(--accent-ink) underline decoration-dashed underline-offset-4"
                : undefined
            }
          >
            {part.seg}
          </span>
          <span className="text-muted-foreground/60">/</span>
        </span>
      ))}
      {parts.length === 0 && (
        <span className="text-muted-foreground/60">class folder/</span>
      )}
      {destName !== fromName && <span>{destName}</span>}
      {hasNew && (
        <span
          title="This folder will be created when the move is approved"
          className={`${chipAccent} ml-1.5`}
        >
          new folder
        </span>
      )}
    </>
  );
}

/** Confidence from sort jobs; chat proposals carry none (NULL) by design.
 *  Canvas says which folder it filed the file in, which is a different kind of
 *  claim from a job's read of the contents — so it says so rather than
 *  borrowing the confidence ladder's vocabulary. */
function ProposalChip({ proposal }: { proposal: MoveProposal }) {
  if (proposal.source === "chat") {
    return (
      <span title="Proposed in chat" className={chipMuted}>
        from chat
      </span>
    );
  }
  if (proposal.source === "canvas") {
    return (
      <span
        title="Downloaded from Canvas, into the folder Canvas keeps it in"
        className={chipAccent}
      >
        from Canvas
      </span>
    );
  }
  if (proposal.source === "by_name") {
    return (
      <span
        title="Its name carries the week — proposed from its row in Materials"
        className={chipAccent}
      >
        by name
      </span>
    );
  }
  switch (proposal.confidence) {
    case "high":
      return (
        <span
          title="The sort job is confident about this destination"
          className={chipAccent}
        >
          high confidence
        </span>
      );
    case "medium":
      return (
        <span
          title="The sort job is fairly sure — worth a glance"
          className={chipAmber}
        >
          medium confidence
        </span>
      );
    case "low":
      return (
        <span
          title="The sort job is guessing — check the destination"
          className={`${chip} text-muted-foreground ring-1 ring-border`}
        >
          low confidence
        </span>
      );
    default:
      return null;
  }
}
