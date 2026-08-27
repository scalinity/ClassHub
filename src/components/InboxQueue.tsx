import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { File } from "lucide-react";

import { useJobs } from "@/lib/jobs";
import { formatSize, type TreeNode } from "@/lib/materials";
import {
  clearDropNotice,
  getSortState,
  INBOX_DIR,
  resolveProposal,
  runSortJob,
  useDragState,
  type MoveProposal,
} from "@/lib/sorter";
import { monoAction } from "@/lib/styles";

const chipBase =
  "shrink-0 rounded px-1.5 py-0.5 font-mono text-[10px] tracking-[0.12em]";

function collectDirs(nodes: readonly TreeNode[], out: string[] = []): string[] {
  for (const node of nodes) {
    if (node.dir) {
      out.push(node.relPath);
      collectDirs(node.children, out);
    }
  }
  return out;
}

/**
 * SPEC §10 steps 3–5: the drop-to-sort confirm queue. One card per pending
 * proposal — destination route, reasoning, confidence — with Approve /
 * Move to… (folder picker) / Leave in inbox; chat-filed proposals render in
 * the same queue. Inbox files nothing has proposed for yet list below, with
 * a manual SORT INBOX trigger when no sort job is active.
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
  const notice = drag.notice?.classId === classId ? drag.notice.message : null;

  // Dismissed files are not counted, but they still keep the section on
  // screen: SORT INBOX is the documented way back out of a dismissal
  // (sorter.rs runs manual sorts over every inbox file, dismissed included),
  // and gating on `count` alone made it unreachable once everything was
  // dismissed — the button's own branch below already contemplates this case.
  if (count === 0 && dismissed.length === 0 && !active && !notice) return null;

  const dirs = tree ? collectDirs(tree) : null;
  const dirSet: ReadonlySet<string> | null = dirs ? new Set(dirs) : null;

  const sortNow = () => {
    setActionError(null);
    runSortJob(classId).catch((e) => setActionError(String(e)));
  };

  return (
    <section className="mt-12" aria-label="Inbox — files waiting to be sorted">
      <div className="flex items-baseline justify-between border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          INBOX
        </h2>
        <div className="flex items-center gap-4">
          {active ? (
            <span className="flex items-center gap-1.5 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
              <span
                aria-hidden
                className="size-1.5 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
              />
              {active.status === "running"
                ? "PROPOSING DESTINATIONS…"
                : "SORT QUEUED"}
            </span>
          ) : (
            (unproposed.length > 0 || dismissed.length > 0) && (
              <button
                type="button"
                onClick={sortNow}
                className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
              >
                SORT INBOX
              </button>
            )
          )}
          {count > 0 && (
            <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
              {count === 1 ? "1 TO SORT" : `${count} TO SORT`}
            </span>
          )}
        </div>
      </div>

      {(notice ?? actionError) && (
        <p className="mt-3 flex items-start gap-2 font-mono text-[11px] text-destructive">
          <span className="min-w-0 flex-1">✕ {notice ?? actionError}</span>
          {notice && (
            <button
              type="button"
              aria-label="Dismiss this notice"
              onClick={clearDropNotice}
              className="shrink-0 cursor-pointer rounded px-1 text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
            >
              ✕
            </button>
          )}
        </p>
      )}
      {lastFailed && unproposed.length > 0 && (
        <p className="mt-3 flex items-center gap-2 font-mono text-[11px] text-destructive">
          <span className="min-w-0 truncate">
            ✕ SORT FAILED — {lastFailed.error ?? "unknown error"}
          </span>
          <button
            type="button"
            onClick={sortNow}
            className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
          >
            RETRY
          </button>
        </p>
      )}

      <div className="mt-4 space-y-2">
        {proposals.map((p) => (
          <ProposalCard key={p.id} proposal={p} dirs={dirs} dirSet={dirSet} />
        ))}
        {unproposed.map((f) => (
          <div key={f.name} className="flex h-8 items-center gap-2 rounded-md px-2">
            <File
              size={14}
              aria-hidden
              className="shrink-0 text-muted-foreground/80"
            />
            <span className="min-w-0 flex-1 truncate text-[13px]">{f.name}</span>
            {active && (
              <span className="shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
                AWAITING PROPOSAL
              </span>
            )}
            <span className="w-14 shrink-0 text-right font-mono text-[11px] text-muted-foreground">
              {formatSize(f.size)}
            </span>
          </div>
        ))}
        {dismissed.map((f) => (
          <div
            key={f.name}
            className="flex h-8 items-center gap-2 rounded-md px-2 opacity-60"
          >
            <File
              size={14}
              aria-hidden
              className="shrink-0 text-muted-foreground/60"
            />
            <span className="min-w-0 flex-1 truncate text-[13px] text-muted-foreground">
              {f.name}
            </span>
            <span
              title="You chose to leave this file in the inbox — SORT INBOX proposes it again"
              className="shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70"
            >
              LEFT IN INBOX
            </span>
            <span className="w-14 shrink-0 text-right font-mono text-[11px] text-muted-foreground">
              {formatSize(f.size)}
            </span>
          </div>
        ))}
      </div>
    </section>
  );
}

function ProposalCard({
  proposal,
  dirs,
  dirSet,
}: {
  proposal: MoveProposal;
  /** null while the tree is loading. */
  dirs: readonly string[] | null;
  dirSet: ReadonlySet<string> | null;
}) {
  // Busy holds until the hub-changed refetch removes the card (or an error
  // re-enables the actions) — a resolved proposal must not be re-clickable.
  const [busy, setBusy] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const source = proposal.sourceRelPath;
  const fileName = source.slice(source.lastIndexOf("/") + 1);
  // MOVE TO… changes only the folder: a rename the proposal carries survives
  // the redirect, so the picker override keeps the destination's file name.
  const destName = proposal.destRelPath.slice(
    proposal.destRelPath.lastIndexOf("/") + 1,
  );
  const fromInbox = source.startsWith(`${INBOX_DIR}/`);
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
    });
  };

  return (
    <div className="rounded-lg border bg-card px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <p className="min-w-0 truncate text-[13px] font-medium">{fileName}</p>
        <ProposalChip proposal={proposal} />
      </div>

      <RouteLine proposal={proposal} dirSet={dirSet} />

      <p className="mt-1.5 text-[12.5px] leading-relaxed text-muted-foreground">
        {proposal.reasoning}
      </p>

      <div className="mt-2.5 flex flex-wrap items-center gap-1">
        <button
          type="button"
          onClick={() => resolve(true)}
          disabled={busy}
          className={`${monoAction} bg-(--accent)/12 text-(--accent) hover:bg-(--accent)/20 disabled:pointer-events-none disabled:opacity-60`}
        >
          {busy ? "WORKING…" : "APPROVE"}
        </button>
        <button
          type="button"
          onClick={() => setPickerOpen((open) => !open)}
          disabled={busy}
          aria-expanded={pickerOpen}
          className={`${monoAction} ${
            pickerOpen
              ? "bg-muted text-foreground"
              : "text-muted-foreground hover:bg-muted hover:text-foreground"
          } disabled:pointer-events-none disabled:opacity-60`}
        >
          MOVE TO…
        </button>
        <button
          type="button"
          onClick={() => resolve(false)}
          disabled={busy}
          className={`${monoAction} text-muted-foreground hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-60`}
        >
          {fromInbox ? "LEAVE IN INBOX" : "DISMISS"}
        </button>
      </div>

      {pickerOpen && !busy && (
        <div className="mt-2 max-h-44 overflow-y-auto rounded-md border p-1">
          {pickerDirs === null ? (
            <p className="px-2 py-1.5 text-[12px] text-muted-foreground">
              Folders are still loading…
            </p>
          ) : pickerDirs.length === 0 ? (
            <p className="px-2 py-1.5 text-[12px] text-muted-foreground">
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
                className="block w-full cursor-pointer truncate rounded px-2 py-1 text-left font-mono text-[11px] text-muted-foreground transition-colors hover:bg-(--accent)/12 hover:text-(--accent) focus-visible:outline-2 focus-visible:outline-(--accent)"
              >
                {dir}/
              </button>
            ))
          )}
        </div>
      )}

      {error && (
        <p className="mt-2 font-mono text-[11px] text-destructive">✕ {error}</p>
      )}
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

  const dest = proposal.destRelPath;
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
    <p className="mt-1.5 flex flex-wrap items-center gap-y-0.5 font-mono text-[11px] leading-relaxed">
      <span className="text-muted-foreground">{fromDir}</span>
      <span aria-hidden className="px-1.5 text-(--accent)">
        →
      </span>
      {parts.map((part, i) => (
        <span key={i} className="flex items-center">
          <span
            className={
              part.isNew
                ? "text-(--accent) underline decoration-dashed underline-offset-4"
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
          className="ml-1.5 rounded bg-(--accent)/12 px-1 py-px text-[9px] font-medium tracking-[0.12em] text-(--accent)"
        >
          NEW FOLDER
        </span>
      )}
    </p>
  );
}

/** Confidence from sort jobs; chat proposals carry none (NULL) by design.
 *  Canvas says which folder it filed the file in, which is a different kind of
 *  claim from a job's read of the contents — so it says so rather than
 *  borrowing the confidence ladder's vocabulary. */
function ProposalChip({ proposal }: { proposal: MoveProposal }) {
  if (proposal.source === "chat") {
    return (
      <span
        title="Proposed in chat"
        className={`${chipBase} bg-muted text-muted-foreground`}
      >
        VIA CHAT
      </span>
    );
  }
  if (proposal.source === "canvas") {
    return (
      <span
        title="Downloaded from Canvas, into the folder Canvas keeps it in"
        className={`${chipBase} bg-(--accent)/12 text-(--accent)`}
      >
        VIA CANVAS
      </span>
    );
  }
  switch (proposal.confidence) {
    case "high":
      return (
        <span
          title="The sort job is confident about this destination"
          className={`${chipBase} bg-(--accent)/12 text-(--accent)`}
        >
          HIGH
        </span>
      );
    case "medium":
      return (
        <span
          title="The sort job is fairly sure — worth a glance"
          className={`${chipBase} bg-class-amber/12 text-class-amber`}
        >
          MEDIUM
        </span>
      );
    case "low":
      return (
        <span
          title="The sort job is guessing — check the destination"
          className={`${chipBase} border text-muted-foreground`}
        >
          LOW
        </span>
      );
    default:
      return null;
  }
}
