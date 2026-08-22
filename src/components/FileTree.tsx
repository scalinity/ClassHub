import { useState } from "react";
import {
  ArrowUpRight,
  ChevronRight,
  CodeXml,
  File,
  FileCode,
  FileText,
  FolderSearch,
  NotebookText,
  NotepadText,
  Presentation,
  RefreshCw,
  type LucideIcon,
} from "lucide-react";

import type { GuideInfo } from "@/lib/guides";
import {
  countFiles,
  formatSize,
  openInDefaultApp,
  revealInFinder,
  type TreeNode,
} from "@/lib/materials";

const KIND_ICONS: Record<string, LucideIcon> = {
  pptx: Presentation,
  pdf: FileText,
  rmd: NotebookText,
  r: FileCode,
  html: CodeXml,
  md: NotepadText,
};

/** Guide state + actions for depth-0 module rows (SPEC §8.1 / M5). */
export interface ModuleGuideControls {
  guides: ReadonlyMap<string, GuideInfo>;
  /** Module scopes with a queued/running module_guide job. */
  activeScopes: ReadonlySet<string>;
  onSynthesize: (scope: string) => void;
  onView: (scope: string) => void;
}

export function FileTree({
  classId,
  nodes,
  onEntryMissing,
  guideControls,
}: {
  classId: number;
  nodes: TreeNode[];
  /** A row action hit a path that vanished from disk — rescan. */
  onEntryMissing: () => void;
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
  guideControls,
}: NodeProps) {
  const isCollapsed = collapsed.has(node.relPath);
  const isModule = depth === 0;
  const fileCount = countFiles(node.children);

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
        {isModule && guideControls && (
          <GuideCluster scope={node.relPath} controls={guideControls} />
        )}
        {isModule && (
          <span className="shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground">
            {fileCount} {fileCount === 1 ? "FILE" : "FILES"}
          </span>
        )}
      </div>

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
              guideControls={guideControls}
            />
          ))}
        </div>
      )}
    </div>
  );
}

const monoAction =
  "shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] transition-colors focus-visible:outline-2 focus-visible:outline-(--accent)";

/**
 * SPEC §8.1 / M5: per-module guide state. Synthesis is manual only; staleness
 * is always visible, the token-costing resynthesize action stays quiet until
 * the guide is actually stale.
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

  if (active) {
    return (
      <span className="flex shrink-0 items-center gap-1.5 px-1.5 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
        <span
          aria-hidden
          className="size-1.5 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
        />
        SYNTHESIZING…
      </span>
    );
  }

  if (!guide) {
    return (
      <button
        type="button"
        onClick={() => controls.onSynthesize(scope)}
        className={`${monoAction} text-muted-foreground hover:bg-(--accent)/12 hover:text-(--accent)`}
      >
        SYNTHESIZE GUIDE
      </button>
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

function FileRow({ node, classId, onEntryMissing }: NodeProps) {
  const Icon = KIND_ICONS[node.kind ?? ""] ?? File;

  const run = (action: Promise<void>) => {
    action.catch(onEntryMissing);
  };

  return (
    <div className="group flex h-8 items-center gap-2 rounded-md px-2 transition-colors hover:bg-muted/60">
      <Icon size={14} aria-hidden className="shrink-0 text-muted-foreground/80" />
      <span className="min-w-0 flex-1 truncate text-[13px]">{node.name}</span>

      <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
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
  );
}
