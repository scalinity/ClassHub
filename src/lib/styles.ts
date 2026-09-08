/**
 * Shared control surfaces from the SPEC §12 design language.
 *
 * Two focus rings, not one: `--accent` is the class colour, and it only exists
 * inside a class-scoped container. Screens that sit outside one (Settings, the
 * chat sidebar and its settings pane, the dashboard header) take the neutral
 * `ring` instead — that is the reason the two variants differ, so reach for
 * the `Neutral` name deliberately rather than letting a copy drift onto the
 * wrong ring. The job pill sets `--accent` for its running job's colour but
 * keeps the neutral ring, since that accent falls back to the muted foreground
 * whenever nothing runs. `--accent-ink` and `--wash` are derived from `--accent`
 * by the base rule in index.css.
 */

// --- Buttons: a control says what happens, in body type ---------------------

/** The one primary action of a surface, in a class scope. */
export const buttonFilled =
  "inline-flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-md bg-(--accent) px-3 text-body font-semibold text-white transition-colors hover:bg-(--accent-ink) focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-50 dark:text-background";

/** `buttonFilled` where no class accent is in scope. */
export const buttonFilledNeutral =
  "inline-flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-md bg-foreground px-3 text-body font-semibold text-background transition-opacity hover:opacity-90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:pointer-events-none disabled:opacity-40";

/** A text action in the class ink, washed on hover. */
export const buttonText =
  "inline-flex h-7 shrink-0 cursor-pointer items-center gap-1 rounded-md px-2 text-body font-medium text-(--accent-ink) transition-colors hover:bg-(--accent)/10 focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-50";

/** A quiet text action: Cancel, Skip, Move to…, a viewer's header. */
export const buttonTextMuted =
  "inline-flex h-7 shrink-0 cursor-pointer items-center gap-1 rounded-md px-2 text-body font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-50";

/** `buttonTextMuted` where no class accent is in scope. */
export const buttonTextNeutral =
  "inline-flex h-7 shrink-0 cursor-pointer items-center gap-1 rounded-md px-2 text-body font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring disabled:pointer-events-none disabled:opacity-50";

/** A tinted status action; the caller adds the tint (amber for a stale guide). */
export const buttonChip =
  "inline-flex h-6 shrink-0 cursor-pointer items-center gap-1 rounded-sm px-2 text-fine font-medium transition-colors focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-50";

/** A 28px icon button in a class scope. */
export const buttonIcon =
  "inline-flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none";

/** `buttonIcon` where no class accent is in scope. */
export const buttonIconNeutral =
  "inline-flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring disabled:pointer-events-none";

// --- Static pieces ----------------------------------------------------------

export const chip =
  "inline-flex h-5 shrink-0 items-center rounded-sm px-1.5 text-fine font-medium";
export const chipAccent = `${chip} bg-(--accent)/12 text-(--accent-ink)`;
export const chipAmber = `${chip} bg-class-amber/12 text-class-amber`;
export const chipMuted = `${chip} bg-muted text-muted-foreground`;

/** Dates, times, counts and sizes: tabular figures in the muted ink. */
export const meta = "text-meta tabular-nums text-muted-foreground";

/** A refusal or failure under a heading or a row — a sentence, never a code. */
export const errorLine = "mt-3 text-body text-destructive";

/** The mark-done control the Deadlines tab's row and the dashboard strip's
 *  chip share: a 15px ring that fills with the accent and a check when done.
 *  The caller appends `checkCircleDone`, or its own idle border — the ring
 *  sits on the paper in the row and inside a class wash on the chip. */
export const checkCircle =
  "flex size-[15px] shrink-0 items-center justify-center rounded-full border transition-colors";
export const checkCircleDone =
  "border-(--accent) bg-(--accent) text-white dark:text-background";

/** The running-job dot; every use keeps the reduced-motion guard. */
export const pulseDot =
  "size-1.5 shrink-0 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none";

/** A line saying what is happening, in the class ink. */
export const statusLine =
  "flex items-center gap-1.5 text-meta text-(--accent-ink)";

// --- Rows and surfaces ------------------------------------------------------

/** A section's list row: a hairline below because the rows are a list. */
export const row =
  "group flex min-h-11 items-center gap-2.5 border-b border-border/70 px-1 transition-colors last:border-b-0 hover:bg-muted/40";

/** A nested tree row or a grade item. */
export const rowDense =
  "group flex min-h-9 items-center gap-2.5 border-b border-border/70 px-1 transition-colors last:border-b-0 hover:bg-muted/40";

/** A card that is a decision: a proposal, a form. */
export const decisionCard = "rounded-xl bg-surface p-4 ring-1 ring-border";

/** A class card: the wash is the card. */
export const washCard = "relative flex flex-col rounded-xl bg-(--wash) p-6";

/** What is read rather than scanned: a notice, an answer, an empty state — the sans at a reading size. */
export const readingText = "text-reading";

/** Single-line text/number/date field in a class scope. */
export const input =
  "h-8 rounded-md border border-border bg-surface px-2.5 text-body placeholder:text-muted-foreground/70 focus-visible:outline-2 focus-visible:outline-(--accent)";

/** `input` where no class accent is in scope. */
export const inputNeutral =
  "h-8 rounded-md border border-border bg-surface px-2.5 text-body placeholder:text-muted-foreground/70 focus-visible:outline-2 focus-visible:outline-ring";

// --- Option rows (Settings, the chat settings pane) -------------------------

/** One choice in a short list; the caller appends `optionRowSelected` or `optionRowIdle`. */
export const optionRow =
  "flex w-full cursor-pointer items-center gap-2.5 rounded-md px-2.5 py-1.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-ring disabled:pointer-events-none";
export const optionRowSelected = "bg-muted/70 ring-1 ring-border";
export const optionRowIdle = "hover:bg-muted/40";
/** The filled dot that marks the chosen row. */
export const optionDot = "size-2 shrink-0 rounded-full";
export const optionDotSelected = "bg-foreground";
export const optionDotIdle = "bg-muted-foreground/30";
