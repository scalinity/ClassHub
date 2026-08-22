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
  type LucideIcon,
} from "lucide-react";

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

export function FileTree({
  classId,
  nodes,
  onEntryMissing,
}: {
  classId: number;
  nodes: TreeNode[];
  /** A row action hit a path that vanished from disk — rescan. */
  onEntryMissing: () => void;
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
}: NodeProps) {
  const isCollapsed = collapsed.has(node.relPath);
  const isModule = depth === 0;
  const fileCount = countFiles(node.children);

  return (
    <div className={isModule ? "not-first:mt-3" : undefined}>
      <button
        type="button"
        aria-expanded={!isCollapsed}
        onClick={() => onToggle(node.relPath)}
        className="flex h-8 w-full cursor-pointer items-center gap-2 rounded-md px-2 text-left transition-colors hover:bg-muted/60 focus-visible:outline-2 focus-visible:outline-(--accent)"
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
        {isModule && (
          <span className="ml-auto shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground">
            {fileCount} {fileCount === 1 ? "FILE" : "FILES"}
          </span>
        )}
      </button>

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
            />
          ))}
        </div>
      )}
    </div>
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
