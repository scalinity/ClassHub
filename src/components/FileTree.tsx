import { useState } from "react";
import {
  ArrowUpRight,
  AudioLines,
  Captions,
  ChevronRight,
  CodeXml,
  File,
  FileCode,
  FileTerminal,
  FileText,
  FolderSearch,
  NotebookPen,
  NotebookText,
  NotepadText,
  Presentation,
  RefreshCw,
  ScrollText,
  Sheet,
  type LucideIcon,
} from "lucide-react";

import { PracticeAction } from "@/components/PracticeAction";
import type { GuideInfo } from "@/lib/guides";
import { WEEKS_DIR, type WeekSlot } from "@/lib/lectures";
import {
  countFiles,
  formatSize,
  openInDefaultApp,
  revealInFinder,
  VIEWABLE_KINDS,
  type TreeNode,
} from "@/lib/materials";
import { proposeWeekFiling } from "@/lib/sorter";
import { monoAction } from "@/lib/styles";

const KIND_ICONS: Record<string, LucideIcon> = {
  pptx: Presentation,
  pdf: FileText,
  docx: ScrollText,
  rmd: NotebookText,
  r: FileCode,
  py: FileTerminal,
  ipynb: NotebookPen,
  html: CodeXml,
  md: NotepadText,
  csv: Sheet,
  caption: Captions,
  media: AudioLines,
};

/**
 * The week folder an entry's name files under (SPEC §10), offered while the
 * course declares the week and the entry sits outside that folder — where
 * the division that reads the week counts it. Proposed from the row, approved
 * in the inbox queue above: an explicit ask, since Canvas may have placed the
 * file where it is. The prefix rule is the affordance; the backend enforces
 * the same rule.
 */
function filingSlot(
  node: TreeNode,
  weekSlots: readonly WeekSlot[],
): WeekSlot | undefined {
  const slot =
    node.week === undefined
      ? undefined
      : weekSlots.find((s) => s.week === node.week);
  return slot && !node.relPath.startsWith(`${WEEKS_DIR}/${slot.folder}/`)
    ? slot
    : undefined;
}

/** Every file under a folder, for its row to read as proposed once each holds a card. */
function filesUnder(node: TreeNode): string[] {
  return node.children.flatMap((c) => (c.dir ? filesUnder(c) : [c.relPath]));
}

/**
 * The by-name action in a row's hover cluster: the click, or the queue's own
 * word while the card waits. Whether a card is waiting is read from the same
 * query the queue renders, so a dismissal or an approval changes it there.
 */
function FilingAction({
  filing,
  proposed,
  proposing,
  title,
  onPropose,
}: {
  filing: WeekSlot;
  proposed: boolean;
  proposing: boolean;
  title: string;
  onPropose: () => void;
}) {
  if (proposed) {
    return (
      <span className="px-1.5 font-mono text-[10px] tracking-[0.14em] text-muted-foreground">
        PROPOSED — SEE INBOX
      </span>
    );
  }
  return (
    <button
      type="button"
      title={title}
      onClick={onPropose}
      disabled={proposing}
      className={`${monoAction} text-(--accent) hover:bg-(--accent)/12 disabled:pointer-events-none disabled:opacity-60`}
    >
      {/* The inbox card's second approval carries this label word for word
          (InboxQueue.tsx, ProposalCard): the card offers the row's own action,
          and the two must read the same. */}
      {proposing
        ? "PROPOSING…"
        : `FILE UNDER WEEK ${String(filing.week).padStart(2, "0")}`}
    </button>
  );
}

/** Guide state + actions for depth-0 module rows (SPEC §8.1 / M5). */
export interface ModuleGuideControls {
  guides: ReadonlyMap<string, GuideInfo>;
  /** Module scopes with a queued/running module_guide job. */
  activeScopes: ReadonlySet<string>;
  /** Scopes with a queued/running practice job (SPEC §8.3). */
  activePracticeScopes: ReadonlySet<string>;
  onSynthesize: (scope: string) => void;
  onPractice: (scope: string) => void;
  onView: (scope: string) => void;
}

export function FileTree({
  classId,
  nodes,
  onEntryMissing,
  onViewFile,
  weekSlots,
  pendingSources,
  guideControls,
}: {
  classId: number;
  nodes: TreeNode[];
  /** A row action hit a path that vanished from disk — rescan. */
  onEntryMissing: () => void;
  /** Open a renderable file in the in-app viewer. */
  onViewFile: (node: TreeNode) => void;
  /** The weeks the course declares, for a file named for one to offer its
   *  week folder (SPEC §10). */
  weekSlots: readonly WeekSlot[];
  /** Sources of the pending move proposals — the queue's own fact, so a row
   *  reads PROPOSED exactly while its card waits. */
  pendingSources: ReadonlySet<string>;
  guideControls?: ModuleGuideControls;
}) {
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());

  const toggle = (relPath: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(relPath)) next.delete(relPath);
      else next.add(relPath);
      return next;
    });

  return (
    <div className="space-y-1">
      {nodes.map((node) => (
        <Node
          key={node.relPath}
          node={node}
          depth={0}
          classId={classId}
          collapsed={collapsed}
          onToggle={toggle}
          onEntryMissing={onEntryMissing}
          onViewFile={onViewFile}
          weekSlots={weekSlots}
          pendingSources={pendingSources}
          guideControls={guideControls}
        />
      ))}
    </div>
  );
}

interface NodeProps {
  node: TreeNode;
  depth: number;
  classId: number;
  collapsed: ReadonlySet<string>;
  onToggle: (relPath: string) => void;
  onEntryMissing: () => void;
  onViewFile: (node: TreeNode) => void;
  weekSlots: readonly WeekSlot[];
  pendingSources: ReadonlySet<string>;
  guideControls?: ModuleGuideControls;
}

function Node(props: NodeProps) {
  const { node } = props;
  return node.dir ? <DirNode {...props} /> : <FileRow {...props} />;
}

function DirNode({
  node,
  depth,
  classId,
  collapsed,
  onToggle,
  onEntryMissing,
  onViewFile,
  weekSlots,
  pendingSources,
  guideControls,
}: NodeProps) {
  const isCollapsed = collapsed.has(node.relPath);
  const isModule = depth === 0;
  const fileCount = countFiles(node.children);
  // A top-level folder's row carries the guide cluster when the folder is
  // named as a division is (`Module 1`), or once a guide exists or a job is
  // running for it, so nothing built or in flight goes unreachable. `Weeks`,
  // `Slides` and `Syllabus` are storage: what they hold reaches a guide
  // through the division that reads it (SPEC §8.3). A folder named some other
  // way — `Unit 1`, `Module 0` — is still a scope chat can ask for by name,
  // and the guide that puts on its row is what brings the cluster back.
  const offersGuide = (controls: ModuleGuideControls) =>
    node.labelled === true ||
    controls.guides.has(node.relPath) ||
    controls.activeScopes.has(node.relPath) ||
    controls.activePracticeScopes.has(node.relPath);

  // A folder named for a week the course declares files what it holds by one
  // click (SPEC §10): one card per file, under the folder's own name in the
  // week folder. Offered while the folder sits outside that folder and holds
  // a file; read as proposed while every file under it has a card — the file
  // row's rule, a card for what is here whatever its destination, since the
  // inbox is where that card is resolved and the backend refuses a click over
  // another route's card by name. Derived only where the action is offered.
  const filing = fileCount > 0 ? filingSlot(node, weekSlots) : undefined;
  const proposed =
    filing !== undefined &&
    filesUnder(node).every((p) => pendingSources.has(p));
  const [proposing, setProposing] = useState(false);
  const [filingError, setFilingError] = useState<string | null>(null);
  const propose = () => {
    setFilingError(null);
    setProposing(true);
    proposeWeekFiling(classId, node.relPath)
      .catch((e) => setFilingError(String(e)))
      .finally(() => setProposing(false));
  };

  return (
    <div className={isModule ? "not-first:mt-3" : undefined}>
      <div className="group flex h-8 w-full items-center gap-2 rounded-md px-2 transition-colors hover:bg-muted/60">
        <button
          type="button"
          aria-expanded={!isCollapsed}
          onClick={() => onToggle(node.relPath)}
          className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 text-left focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <ChevronRight
            size={13}
            strokeWidth={2.5}
            aria-hidden
            className={
              "shrink-0 transition-transform duration-150 " +
              (isCollapsed ? "" : "rotate-90 ") +
              (isModule ? "text-(--accent)" : "text-muted-foreground/70")
            }
          />
          <span
            className={
              "min-w-0 truncate " +
              (isModule ? "text-[14px] font-semibold" : "text-[13px] font-medium")
            }
          >
            {node.name}
          </span>
        </button>
        {filing && (
          <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
            <FilingAction
              filing={filing}
              proposed={proposed}
              proposing={proposing}
              title={`Propose moving its files into ${WEEKS_DIR}/${filing.folder}/${node.name}, where ${filing.unitName} reads them`}
              onPropose={propose}
            />
          </span>
        )}
        {isModule && guideControls && offersGuide(guideControls) && (
          <GuideCluster scope={node.relPath} controls={guideControls} />
        )}
        {isModule && (
          <span className="shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground">
            {fileCount} {fileCount === 1 ? "FILE" : "FILES"}
          </span>
        )}
      </div>
      {filingError && (
        <p className="ml-8 pb-1 font-mono text-[11px] text-destructive">
          NOT PROPOSED — {filingError}
        </p>
      )}

      {!isCollapsed && node.children.length > 0 && (
        <div className="ml-[15px] border-l border-border pl-2">
          {node.children.map((child) => (
            <Node
              key={child.relPath}
              node={child}
              depth={depth + 1}
              classId={classId}
              collapsed={collapsed}
              onToggle={onToggle}
              onEntryMissing={onEntryMissing}
              onViewFile={onViewFile}
              weekSlots={weekSlots}
              pendingSources={pendingSources}
              guideControls={guideControls}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * SPEC §8.1 / §8.3: a folder guide's state, on the rows `DirNode` offers it —
 * a top-level folder named as a division is, or one with a guide or a job
 * already. Synthesis is manual only; staleness is always visible, the
 * token-costing resynthesize action stays quiet until the guide is actually
 * stale. The practice exam draws on the same sources, so it sits beside the
 * guide in every state the cluster appears in.
 */
function GuideCluster({
  scope,
  controls,
}: {
  scope: string;
  controls: ModuleGuideControls;
}) {
  const guide = controls.guides.get(scope);
  const active = controls.activeScopes.has(scope);
  const practice = (
    <PracticeAction
      active={controls.activePracticeScopes.has(scope)}
      onSelect={() => controls.onPractice(scope)}
    />
  );

  if (active) {
    return (
      <span className="flex shrink-0 items-center gap-0.5">
        <span className="flex shrink-0 items-center gap-1.5 px-1.5 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
          <span
            aria-hidden
            className="size-1.5 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
          />
          SYNTHESIZING…
        </span>
        {practice}
      </span>
    );
  }

  if (!guide) {
    return (
      <span className="flex shrink-0 items-center gap-0.5">
        <button
          type="button"
          onClick={() => controls.onSynthesize(scope)}
          className={`${monoAction} text-muted-foreground hover:bg-(--accent)/12 hover:text-(--accent)`}
        >
          SYNTHESIZE GUIDE
        </button>
        {practice}
      </span>
    );
  }

  return (
    <span className="flex shrink-0 items-center gap-0.5">
      {guide.stale ? (
        <button
          type="button"
          title="Sources changed since this guide was generated"
          onClick={() => controls.onSynthesize(scope)}
          className={`${monoAction} bg-class-amber/12 text-class-amber hover:bg-class-amber/20`}
        >
          STALE — RESYNTHESIZE
        </button>
      ) : (
        <button
          type="button"
          title="Resynthesize guide"
          aria-label={`Resynthesize the ${scope} guide`}
          onClick={() => controls.onSynthesize(scope)}
          className="cursor-pointer rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-muted hover:text-foreground focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-(--accent) group-focus-within:opacity-100 group-hover:opacity-100"
        >
          <RefreshCw size={12} aria-hidden />
        </button>
      )}
      {practice}
      <button
        type="button"
        onClick={() => controls.onView(scope)}
        className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
      >
        VIEW GUIDE
      </button>
    </span>
  );
}

function FileRow({
  node,
  classId,
  onEntryMissing,
  onViewFile,
  weekSlots,
  pendingSources,
}: NodeProps) {
  const Icon = KIND_ICONS[node.kind ?? ""] ?? File;
  const viewable = VIEWABLE_KINDS.has(node.kind ?? "");
  // The week folder its name files under, if any (SPEC §10).
  const filing = filingSlot(node, weekSlots);
  // The queue's fact: a card left pending from an earlier session shows here.
  const proposed = pendingSources.has(node.relPath);
  const [proposing, setProposing] = useState(false);
  const [filingError, setFilingError] = useState<string | null>(null);

  const run = (action: Promise<void>) => {
    action.catch(onEntryMissing);
  };

  const propose = () => {
    setFilingError(null);
    setProposing(true);
    proposeWeekFiling(classId, node.relPath)
      .catch((e) => setFilingError(String(e)))
      .finally(() => setProposing(false));
  };

  return (
    <div>
      <div className="group flex h-8 items-center gap-2 rounded-md px-2 transition-colors hover:bg-muted/60">
        <Icon size={14} aria-hidden className="shrink-0 text-muted-foreground/80" />
        <button
          type="button"
          title={viewable ? `View ${node.name}` : `Open ${node.name} in its default app`}
          onClick={() =>
            viewable
              ? onViewFile(node)
              : run(openInDefaultApp(classId, node.relPath))
          }
          className="min-w-0 flex-1 cursor-pointer truncate text-left text-[13px] transition-colors hover:text-(--accent) focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          {node.name}
        </button>

        <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
          {filing && (
            <FilingAction
              filing={filing}
              proposed={proposed}
              proposing={proposing}
              title={`Propose moving it into ${WEEKS_DIR}/${filing.folder}, where ${filing.unitName} reads it`}
              onPropose={propose}
            />
          )}
          <button
            type="button"
            title="Show in Finder"
            aria-label={`Show ${node.name} in Finder`}
            onClick={() => run(revealInFinder(classId, node.relPath))}
            className="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
          >
            <FolderSearch size={13} aria-hidden />
          </button>
          <button
            type="button"
            title="Open in default app"
            aria-label={`Open ${node.name} in its default app`}
            onClick={() => run(openInDefaultApp(classId, node.relPath))}
            className="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
          >
            <ArrowUpRight size={13} aria-hidden />
          </button>
        </span>

        <span className="w-14 shrink-0 text-right font-mono text-[11px] text-muted-foreground">
          {formatSize(node.size ?? 0)}
        </span>
      </div>
      {filingError && (
        <p className="ml-8 pb-1 font-mono text-[11px] text-destructive">
          NOT PROPOSED — {filingError}
        </p>
      )}
    </div>
  );
}
