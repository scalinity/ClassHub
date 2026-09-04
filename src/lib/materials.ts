import { invoke } from "@tauri-apps/api/core";

export interface TreeNode {
  name: string;
  relPath: string;
  dir: boolean;
  kind?: string; // pptx | pdf | docx | rmd | r | py | ipynb | html | md | csv | caption | media | other
  size?: number;
  /** Class-relative extract made from the file as it is now; absent until the pipeline has written one. */
  extractRelPath?: string;
  /** A folder named the way a division is — `Module 1` — the one kind the tree offers a guide on (SPEC §8.3); absent on storage folders and files. */
  labelled?: boolean;
  /** The week the name files under — `CAI6734_Week2_….pdf` is 2, the folder `Week 3 Coding Material` is 3, and on a course that reads modules as weeks `…Module3….pptx` is 3 — for the row to offer its week folder (SPEC §10); absent on a week folder under `Weeks/` and on a name carrying none. */
  week?: number;
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

/** Raw text of a class file for the in-app viewer (backend caps the size). */
export function readClassFile(classId: number, relPath: string): Promise<string> {
  return invoke<string>("read_class_file", { classId, relPath });
}

/**
 * The absolute path the viewer frames for a PDF, or for a slide deck its
 * converted twin — `null` while the twin is missing or made from an older
 * deck, which is when the deck opens in its default app instead. A rejection
 * is a failure, not a missing twin.
 */
export function pdfViewPath(
  classId: number,
  relPath: string,
): Promise<string | null> {
  return invoke<string | null>("pdf_view_path", { classId, relPath });
}

/** A file in an app-managed directory (notes, practice exams); disk is truth. */
export interface ManagedFile {
  name: string;
  relPath: string; // class-relative, e.g. "Notes/Week 3.md"
  modifiedAt: number; // unix seconds
}

/** Markdown notes in the class's Notes folder, newest first. */
export function listNotes(classId: number): Promise<ManagedFile[]> {
  return invoke<ManagedFile[]>("list_notes", { classId });
}

/**
 * The notes editor's save; a replaced version is kept in the audit log.
 * `relPath` targets an exact existing file; without it the title names one.
 */
export function saveNote(
  classId: number,
  title: string,
  content: string,
  relPath?: string,
): Promise<{ relPath: string; created: boolean }> {
  return invoke("save_note", { classId, title, content, relPath });
}

/** Generated practice exams in Study Guides/Practice, newest first. */
export function listPractice(classId: number): Promise<ManagedFile[]> {
  return invoke<ManagedFile[]>("list_practice", { classId });
}

/**
 * Kinds the in-app viewer can render; the rest open in their default app.
 * A PDF and a converted deck are framed through the asset protocol, a
 * notebook opens as its extract, scripts and CSVs as text (SPEC §12).
 */
export const VIEWABLE_KINDS: ReadonlySet<string> = new Set([
  "html",
  "md",
  "rmd",
  "r",
  "py",
  "csv",
  "ipynb",
  "pdf",
  "pptx",
]);

export function countFiles(nodes: TreeNode[]): number {
  return nodes.reduce(
    (total, node) => total + (node.dir ? countFiles(node.children) : 1),
    0,
  );
}

/** 1234 -> "1.2 KB"; whole numbers at >= 10 of a unit. */
/** Mirrors `format_size` in src-tauri/src/tools.rs — same thresholds, same
 *  "whole numbers at >= 10 of a unit" rule. Both sides genuinely need it (Rust
 *  builds prompt text, this renders the inbox); change one, change the other. */
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
