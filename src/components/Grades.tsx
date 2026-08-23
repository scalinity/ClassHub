import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Pencil, Plus, Trash2 } from "lucide-react";

import {
  deleteGradeCategory,
  deleteGradeItem,
  formatPercent,
  formatScore,
  listGrades,
  saveGradeCategory,
  saveGradeItem,
  type GradeCategory,
  type GradeItem,
} from "@/lib/grades";

const monoAction =
  "shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] transition-colors focus-visible:outline-2 focus-visible:outline-(--accent)";

const inputBase =
  "h-8 rounded-md border bg-transparent px-2.5 text-[13px] focus-visible:outline-2 focus-visible:outline-(--accent)";

const iconAction =
  "cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted focus-visible:outline-2 focus-visible:outline-(--accent)";

type Editing =
  | { kind: "category"; target: GradeCategory | "new" }
  | { kind: "item"; categoryId: number; target: GradeItem | "new" }
  | null;

/**
 * SPEC §11 — the weighted grade tracker. Categories mirror the syllabus
 * weighting; items are scores as they come back. The computed number is the
 * same one the class card and the chat overview show (grades.rs math:
 * renormalized over categories that have items).
 */
export function GradesSection({ classId }: { classId: number }) {
  const { data } = useQuery({
    queryKey: ["grades", classId],
    queryFn: () => listGrades(classId),
  });

  const [editing, setEditing] = useState<Editing>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const categories = data?.categories ?? [];
  const weightTotal = data?.weightTotal ?? 0;
  const offBy = Math.round((weightTotal - 100) * 10) / 10;
  const showWeightWarning = data !== undefined && categories.length > 0 && offBy !== 0;

  return (
    <section className="mt-12" aria-label="Grades">
      <div className="flex items-baseline justify-between border-b pb-3">
        <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          GRADES
        </h2>
        <div className="flex items-baseline gap-4">
          <button
            type="button"
            onClick={() => setEditing({ kind: "category", target: "new" })}
            className={`${monoAction} text-(--accent) hover:bg-(--accent)/12`}
          >
            ADD CATEGORY
          </button>
          {data?.currentGrade != null && (
            <span className="font-mono text-[11px] font-medium tracking-[0.14em] text-(--accent)">
              CURRENT {formatPercent(data.currentGrade)}
            </span>
          )}
        </div>
      </div>

      {actionError && (
        <p className="mt-3 font-mono text-[11px] text-destructive">
          ✕ {actionError}
        </p>
      )}
      {showWeightWarning && (
        <p className="mt-3 font-mono text-[11px] tracking-[0.06em] text-class-amber">
          WEIGHTS SUM {formatPercent(weightTotal)} —{" "}
          {offBy < 0
            ? `${formatPercent(-offBy)} UNASSIGNED`
            : `${formatPercent(offBy)} OVER 100`}
        </p>
      )}

      {editing?.kind === "category" && (
        // Keyed by target: the form seeds its fields in useState initializers,
        // so switching edit targets must remount it (the M10 lesson).
        <CategoryForm
          key={editing.target === "new" ? "new" : editing.target.id}
          classId={classId}
          category={editing.target === "new" ? null : editing.target}
          onClose={() => setEditing(null)}
        />
      )}

      {data !== undefined && categories.length === 0 && editing === null && (
        <p className="py-2 text-[13px] text-muted-foreground">
          No grades tracked yet — add the syllabus categories and their
          weights, then record scores as they come back. The chat can fill
          this in too.
        </p>
      )}

      <div className="mt-3 space-y-2">
        {categories.map((category) => (
          <div key={category.id}>
            <CategoryRow
              category={category}
              onAddItem={() =>
                setEditing({ kind: "item", categoryId: category.id, target: "new" })
              }
              onEdit={() => setEditing({ kind: "category", target: category })}
              onError={setActionError}
            />
            {(category.items.length > 0 ||
              (editing?.kind === "item" && editing.categoryId === category.id)) && (
              <div className="mt-0.5 ml-[13px] space-y-0.5 border-l pl-3">
                {category.items.map((item) =>
                  editing?.kind === "item" &&
                  editing.target !== "new" &&
                  editing.target.id === item.id ? (
                    <ItemForm
                      key={item.id}
                      categoryId={category.id}
                      item={item}
                      onClose={() => setEditing(null)}
                    />
                  ) : (
                    <ItemRow
                      key={item.id}
                      item={item}
                      onEdit={() =>
                        setEditing({
                          kind: "item",
                          categoryId: category.id,
                          target: item,
                        })
                      }
                      onError={setActionError}
                    />
                  ),
                )}
                {editing?.kind === "item" &&
                  editing.categoryId === category.id &&
                  editing.target === "new" && (
                    <ItemForm
                      key="new"
                      categoryId={category.id}
                      item={null}
                      onClose={() => setEditing(null)}
                    />
                  )}
              </div>
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

function CategoryRow({
  category,
  onAddItem,
  onEdit,
  onError,
}: {
  category: GradeCategory;
  onAddItem: () => void;
  onEdit: () => void;
  onError: (message: string) => void;
}) {
  const [busy, setBusy] = useState(false);

  const remove = () => {
    setBusy(true);
    deleteGradeCategory(category.id).catch((e) => {
      onError(String(e));
      setBusy(false);
    });
  };

  return (
    <div className="group flex h-9 items-center gap-2.5 rounded-md px-2 transition-colors hover:bg-muted/60">
      <span className="w-10 shrink-0 text-right font-mono text-[11px] font-medium text-(--accent)">
        {formatPercent(category.weight)}
      </span>
      <span className="min-w-0 flex-1 truncate text-[13px] font-medium">
        {category.name}
      </span>
      <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
        {category.percent == null
          ? "NO SCORES YET"
          : `${category.items.length > 1 ? `${category.items.length} · ` : ""}${formatPercent(category.percent)}`}
      </span>
      <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
        <button
          type="button"
          title={`Record a score in ${category.name}`}
          aria-label={`Record a score in ${category.name}`}
          disabled={busy}
          onClick={onAddItem}
          className={`${iconAction} hover:text-(--accent)`}
        >
          <Plus size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Edit category"
          aria-label={`Edit ${category.name}`}
          disabled={busy}
          onClick={onEdit}
          className={`${iconAction} hover:text-foreground`}
        >
          <Pencil size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Delete category and its scores (kept in the audit log)"
          aria-label={`Delete ${category.name}`}
          disabled={busy}
          onClick={remove}
          className={`${iconAction} hover:text-destructive`}
        >
          <Trash2 size={12} aria-hidden />
        </button>
      </span>
    </div>
  );
}

function ItemRow({
  item,
  onEdit,
  onError,
}: {
  item: GradeItem;
  onEdit: () => void;
  onError: (message: string) => void;
}) {
  const [busy, setBusy] = useState(false);

  const remove = () => {
    setBusy(true);
    deleteGradeItem(item.id).catch((e) => {
      onError(String(e));
      setBusy(false);
    });
  };

  return (
    <div className="group flex h-8 items-center gap-2.5 rounded-md px-2 transition-colors hover:bg-muted/60">
      <span className="min-w-0 flex-1 truncate text-[13px]">{item.name}</span>
      <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
        {formatScore(item.score, item.maxScore)}
      </span>
      <span className="w-12 shrink-0 text-right font-mono text-[11px] text-muted-foreground/70">
        {formatPercent((item.score / item.maxScore) * 100)}
      </span>
      <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
        <button
          type="button"
          title="Edit score"
          aria-label={`Edit ${item.name}`}
          disabled={busy}
          onClick={onEdit}
          className={`${iconAction} hover:text-foreground`}
        >
          <Pencil size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Delete score (kept in the audit log)"
          aria-label={`Delete ${item.name}`}
          disabled={busy}
          onClick={remove}
          className={`${iconAction} hover:text-destructive`}
        >
          <Trash2 size={12} aria-hidden />
        </button>
      </span>
    </div>
  );
}

function CategoryForm({
  classId,
  category,
  onClose,
}: {
  classId: number;
  /** null creates; a row prefills and amends. */
  category: GradeCategory | null;
  onClose: () => void;
}) {
  const [name, setName] = useState(category?.name ?? "");
  const [weight, setWeight] = useState(
    category ? String(category.weight) : "",
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const parsedWeight = Number(weight);
  const canSave =
    name.trim() !== "" &&
    weight.trim() !== "" &&
    Number.isFinite(parsedWeight) &&
    parsedWeight >= 0 &&
    parsedWeight <= 100;

  const save = () => {
    setBusy(true);
    setError(null);
    saveGradeCategory({
      classId,
      id: category?.id,
      name: name.trim(),
      weight: parsedWeight,
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
        {category ? "EDIT CATEGORY" : "NEW CATEGORY"}
      </p>
      <div className="mt-2.5 flex flex-wrap items-center gap-2">
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Category — Homework, Quizzes, Final…"
          aria-label="Category name"
          autoFocus
          className={`${inputBase} min-w-40 flex-1`}
        />
        <label className="flex items-center gap-1.5">
          <input
            type="number"
            value={weight}
            onChange={(e) => setWeight(e.target.value)}
            min={0}
            max={100}
            step="any"
            placeholder="30"
            aria-label="Weight as a percentage of the final grade"
            className={`${inputBase} w-20 font-mono text-[12px]`}
          />
          <span className="font-mono text-[11px] text-muted-foreground">
            % OF GRADE
          </span>
        </label>
      </div>
      <div className="mt-3 flex items-center gap-1">
        <button
          type="button"
          onClick={save}
          disabled={busy || !canSave}
          className={`${monoAction} bg-(--accent)/12 text-(--accent) hover:bg-(--accent)/20 disabled:pointer-events-none disabled:opacity-60`}
        >
          {busy ? "SAVING…" : "SAVE CATEGORY"}
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

function ItemForm({
  categoryId,
  item,
  onClose,
}: {
  categoryId: number;
  /** null creates; a row prefills and amends. */
  item: GradeItem | null;
  onClose: () => void;
}) {
  const [name, setName] = useState(item?.name ?? "");
  const [score, setScore] = useState(item ? String(item.score) : "");
  const [maxScore, setMaxScore] = useState(item ? String(item.maxScore) : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const parsedScore = Number(score);
  const parsedMax = Number(maxScore);
  const canSave =
    name.trim() !== "" &&
    score.trim() !== "" &&
    maxScore.trim() !== "" &&
    Number.isFinite(parsedScore) &&
    Number.isFinite(parsedMax) &&
    parsedScore >= 0 &&
    parsedMax > 0;

  const save = () => {
    setBusy(true);
    setError(null);
    saveGradeItem({
      categoryId,
      id: item?.id,
      name: name.trim(),
      score: parsedScore,
      maxScore: parsedMax,
    })
      .then(onClose)
      .catch((e) => {
        setError(String(e));
        setBusy(false);
      });
  };

  return (
    <div className="rounded-md border bg-card px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="What was graded — Quiz 1, Homework 2…"
          aria-label="Item name"
          autoFocus
          className={`${inputBase} min-w-36 flex-1`}
        />
        <input
          type="number"
          value={score}
          onChange={(e) => setScore(e.target.value)}
          min={0}
          step="any"
          placeholder="9"
          aria-label="Score earned"
          className={`${inputBase} w-16 font-mono text-[12px]`}
        />
        <span className="font-mono text-[12px] text-muted-foreground">/</span>
        <input
          type="number"
          value={maxScore}
          onChange={(e) => setMaxScore(e.target.value)}
          min={0}
          step="any"
          placeholder="10"
          aria-label="Maximum score"
          className={`${inputBase} w-16 font-mono text-[12px]`}
        />
        <button
          type="button"
          onClick={save}
          disabled={busy || !canSave}
          className={`${monoAction} bg-(--accent)/12 text-(--accent) hover:bg-(--accent)/20 disabled:pointer-events-none disabled:opacity-60`}
        >
          {busy ? "SAVING…" : "SAVE"}
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
