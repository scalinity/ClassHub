import { invoke } from "@tauri-apps/api/core";

export interface TreeNode {
  name: string;
  relPath: string;
  dir: boolean;
  kind?: string; // pptx | pdf | rmd | r | html | md | other
  size?: number;
  children: TreeNode[];
}

/** Scans the class folder on disk, syncs the files table, returns the tree. */
export function scanClass(classId: number): Promise<TreeNode[]> {
  return invoke<TreeNode[]>("scan_class", { classId });
}

/** relPath "" targets the class folder itself. */
export function revealInFinder(classId: number, relPath: string): Promise<void> {
  return invoke("reveal_in_finder", { classId, relPath });
}

export function openInDefaultApp(classId: number, relPath: string): Promise<void> {
  return invoke("open_in_default_app", { classId, relPath });
}

export function countFiles(nodes: TreeNode[]): number {
  return nodes.reduce(
    (total, node) => total + (node.dir ? countFiles(node.children) : 1),
    0,
  );
}

/** 1234 -> "1.2 KB"; whole numbers at >= 10 of a unit. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes;
  let unit = "B";
  for (const next of units) {
    if (value < 1024) break;
    value /= 1024;
    unit = next;
  }
  return `${value >= 10 ? Math.round(value) : value.toFixed(1)} ${unit}`;
}
