import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Pencil, Plus, Trash2 } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import {
  CANVAS_CATEGORY_TAG,
  CANVAS_ITEM_TAG,
  deleteGradeCategory,
  deleteGradeItem,
  formatPercent,
  formatScore,
  listGrades,
  projectionLine,
  saveGradeCategory,
  saveGradeItem,
  type GradeCategory,
  type GradeItem,
} from "@/lib/grades";
import {
  buttonFilled,
  buttonIcon,
  buttonText,
  buttonTextMuted,
  decisionCard,
  errorLine,
  input,
  meta,
  readingText,
  row,
  rowDense,
} from "@/lib/styles";

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
  const { data, error } = useQuery({
    queryKey: ["grades", classId],
    queryFn: () => listGrades(classId),
  });

  const [editing, setEditing] = useState<Editing>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const categories = data?.categories ?? [];
  const weightTotal = data?.weightTotal ?? 0;
  // Same tolerance the backend applies (grades.rs WEIGHT_EPSILON), so a set
  // summing to 100.03 cannot read clean here and flagged in chat.
  const offBy = weightTotal - 100;
  const showWeightWarning =
    data !== undefined && categories.length > 0 && Math.abs(offBy) >= 0.01;

  return (
    <section id="grades" className="mt-14 scroll-mt-20" aria-label="Grades">
      <SectionHeading
        title="Grades"
        count={
          data?.currentGrade != null
            ? `Current ${formatPercent(data.currentGrade)}`
            : undefined
        }
        actions={
          <button
            type="button"
            onClick={() => setEditing({ kind: "category", target: "new" })}
            className={buttonText}
          >
            Add category
          </button>
        }
      >
        {error != null && (
          <p className={errorLine}>The grades didn't load: {String(error)}</p>
        )}
        {actionError && <p className={errorLine}>{actionError}</p>}
        {/* Where the grade could land (SPEC §11), once a score exists. */}
        {data?.projection && (
          <p className="mt-3 max-w-xl text-body text-muted-foreground">
            {projectionLine(data.projection)}
          </p>
        )}
        {showWeightWarning && (
          <p className="mt-3 text-body text-class-amber">
            Weights sum to {formatPercent(weightTotal)},{" "}
            {offBy < 0
              ? `${formatPercent(-offBy)} unassigned`
              : `${formatPercent(offBy)} over 100`}
          </p>
        )}
      </SectionHeading>

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
        <p className={`max-w-xl py-2 ${readingText} text-muted-foreground`}>
          No grades tracked yet — sync Canvas to bring in the course's
          categories and every posted score, or add the syllabus categories
          and record scores by hand. The chat can fill this in too.
        </p>
      )}

      <div className="mt-3">
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
              <div className="my-1 ml-[13px] border-l border-border/70 pl-3">
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
    // Cleared on both paths. On success the row usually unmounts when the
    // hub-changed refetch lands, but "usually" is not a lifecycle: if the
    // refetch is slow or the row survives, a stuck flag leaves the control
    // disabled with nothing to release it.
    deleteGradeCategory(category.id)
      .catch((e) => onError(String(e)))
      .finally(() => setBusy(false));
  };

  return (
    <div className={row}>
      <span className="w-10 shrink-0 text-right text-meta font-medium tabular-nums text-(--accent-ink)">
        {formatPercent(category.weight)}
      </span>
      <span className="min-w-0 flex-1 truncate text-title">{category.name}</span>
      {category.canvasGroupId !== null && (
        <span
          title={CANVAS_CATEGORY_TAG.title}
          className="shrink-0 text-fine text-muted-foreground"
        >
          {CANVAS_CATEGORY_TAG.label}
        </span>
      )}
      <span className="flex shrink-0 items-center gap-0.5">
        <button
          type="button"
          title={`Record a score in ${category.name}`}
          aria-label={`Record a score in ${category.name}`}
          disabled={busy}
          onClick={onAddItem}
          className={`${buttonIcon} hover:text-(--accent-ink)`}
        >
          <Plus size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Edit category"
          aria-label={`Edit ${category.name}`}
          disabled={busy}
          onClick={onEdit}
          className={buttonIcon}
        >
          <Pencil size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Delete category and its scores (kept in the audit log)"
          aria-label={`Delete ${category.name}`}
          disabled={busy}
          onClick={remove}
          className={`${buttonIcon} hover:text-destructive`}
        >
          <Trash2 size={12} aria-hidden />
        </button>
      </span>
      <span className={`shrink-0 ${meta}`}>
        {category.percent == null
          ? "No scores yet"
          : `${category.items.length > 1 ? `${category.items.length} · ` : ""}${formatPercent(category.percent)}`}
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
    <div className={rowDense}>
      <span className="min-w-0 flex-1 truncate text-body">{item.name}</span>
      {item.canvasAssignmentId !== null && (
        <span
          title={CANVAS_ITEM_TAG.title}
          className="shrink-0 text-fine text-muted-foreground"
        >
          {CANVAS_ITEM_TAG.label}
        </span>
      )}
      <span className="flex shrink-0 items-center gap-0.5">
        <button
          type="button"
          title="Edit score"
          aria-label={`Edit ${item.name}`}
          disabled={busy}
          onClick={onEdit}
          className={buttonIcon}
        >
          <Pencil size={12} aria-hidden />
        </button>
        <button
          type="button"
          title="Delete score (kept in the audit log)"
          aria-label={`Delete ${item.name}`}
          disabled={busy}
          onClick={remove}
          className={`${buttonIcon} hover:text-destructive`}
        >
          <Trash2 size={12} aria-hidden />
        </button>
      </span>
      <span className={`shrink-0 ${meta}`}>
        {formatScore(item.score, item.maxScore)}
      </span>
      <span className={`w-12 shrink-0 text-right ${meta}`}>
        {formatPercent((item.score / item.maxScore) * 100)}
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
    <div className={`${decisionCard} mt-4`}>
      <p className="text-[15px] font-semibold">
        {category ? "Edit category" : "New category"}
      </p>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Category — Homework, Quizzes, Final…"
          aria-label="Category name"
          autoFocus
          className={`${input} min-w-40 flex-1`}
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
            className={`${input} w-20 tabular-nums`}
          />
          <span className={meta}>% of grade</span>
        </label>
      </div>
      <div className="mt-3 flex items-center gap-1">
        <button
          type="button"
          onClick={save}
          disabled={busy || !canSave}
          className={buttonFilled}
        >
          {busy ? "Saving…" : "Save category"}
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
    <div className="my-1 rounded-md bg-surface p-3 ring-1 ring-border">
      <div className="flex flex-wrap items-center gap-2">
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="What was graded — Quiz 1, Homework 2…"
          aria-label="Item name"
          autoFocus
          className={`${input} min-w-36 flex-1`}
        />
        <input
          type="number"
          value={score}
          onChange={(e) => setScore(e.target.value)}
          min={0}
          step="any"
          placeholder="9"
          aria-label="Score earned"
          className={`${input} w-16 tabular-nums`}
        />
        <span className={meta}>/</span>
        <input
          type="number"
          value={maxScore}
          onChange={(e) => setMaxScore(e.target.value)}
          min={0}
          step="any"
          placeholder="10"
          aria-label="Maximum score"
          className={`${input} w-16 tabular-nums`}
        />
        <button
          type="button"
          onClick={save}
          disabled={busy || !canSave}
          className={buttonFilled}
        >
          {busy ? "Saving…" : "Save"}
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
