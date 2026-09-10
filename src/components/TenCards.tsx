import { useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";

import { SectionHeading } from "@/components/SectionHeading";
import {
  answerCard,
  dueCardsQuery,
  pickCard,
  type CardInfo,
  type Verdict,
} from "@/lib/cards";
import { CLASS_ACCENTS } from "@/lib/classes";
import { daysUntil } from "@/lib/schedule";
import {
  buttonText,
  buttonTextMuted,
  errorLine,
  meta,
  readingText,
  washCard,
} from "@/lib/styles";

/**
 * When an answered card comes back, in the words the receipt uses. Read off
 * the due date the answer returned rather than recomputed from the box, so
 * the line cannot drift from the schedule the card actually got.
 */
function backIn(dueOn: string | null): string {
  if (dueOn === null) return "back soon";
  const days = daysUntil(dueOn);
  if (days <= 0) return "back today";
  if (days === 1) return "back tomorrow";
  return `back in ${days} days`;
}

/**
 * SPEC §12 — a card with options: pick one, and the app marks it.
 *
 * The whole point is that the commitment comes before the answer is visible.
 * Revealing the back and then judging whether you knew it is hindsight — the
 * answer is already on screen — so nothing here shows the back until a choice
 * has been made, and the box moves from the pick rather than from a claim.
 *
 * The verdict stays until `Next card`, because the explanation under it is the
 * part worth reading, especially when the pick was wrong.
 */
function Choices({
  card,
  onDone,
  onFailed,
}: {
  card: CardInfo;
  /** Called when the reader moves on, not when they pick — the verdict has to
   *  stay on screen long enough to read. */
  onDone: (verdict: Verdict) => void;
  onFailed: (message: string | null) => void;
}) {
  const [verdict, setVerdict] = useState<Verdict | null>(null);
  const [picked, setPicked] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const options = card.choices ?? [];

  const choose = (index: number) => {
    if (busy || verdict !== null) return;
    setBusy(true);
    setPicked(index);
    onFailed(null);
    pickCard(card.id, index)
      .then((answered) => {
        setVerdict(answered);
        setBusy(false);
      })
      .catch((e) => {
        setPicked(null);
        setBusy(false);
        onFailed(String(e));
      });
  };

  return (
    <>
      <ul className="mt-4 max-w-2xl space-y-1.5">
        {options.map((option, index) => {
          // Before the pick every option is neutral. After it, the true one
          // takes the class's own colour and a wrong pick takes the
          // destructive one — the two the app already uses for "this is the
          // thing" and "this went wrong". Nothing new is invented, and the
          // mark carries a glyph so the state is not colour alone.
          const isTrue = verdict !== null && index === verdict.correct;
          const isWrongPick = verdict !== null && index === picked && !verdict.right;
          const state = isTrue
            ? "border-(--accent) bg-(--wash) text-foreground"
            : isWrongPick
              ? "border-destructive text-destructive"
              : verdict !== null
                ? "border-transparent text-muted-foreground/60"
                : "border-border/70 hover:border-(--accent) hover:bg-(--accent)/8";
          return (
            <li key={index}>
              <button
                type="button"
                disabled={busy || verdict !== null}
                onClick={() => choose(index)}
                className={`flex w-full cursor-pointer items-start gap-2.5 rounded-lg border px-3 py-2.5 text-left text-body transition-colors focus-visible:outline-2 focus-visible:outline-(--accent) disabled:cursor-default ${state}`}
              >
                {/* The mark is a glyph, so the state is never colour alone;
                    the word beside it is what a screen reader gets, since a
                    tick is not one. Not `aria-pressed`: this is a choice made
                    once, not a toggle, and announcing it as one is wrong. */}
                <span aria-hidden className="mt-px w-3 shrink-0 text-center text-fine">
                  {isTrue ? "✓" : isWrongPick ? "✕" : ""}
                </span>
                {isTrue && <span className="sr-only">Correct answer. </span>}
                {isWrongPick && <span className="sr-only">Your answer, wrong. </span>}
                <span className="min-w-0 flex-1">{option}</span>
              </button>
            </li>
          );
        })}
      </ul>

      {verdict !== null && (
        <div className="mt-4 animate-in fade-in slide-in-from-top-1 duration-200 motion-reduce:animate-none">
          <p className={`border-t border-(--accent)/25 pt-4 ${readingText} text-muted-foreground`}>
            {card.back}
          </p>
          {card.source && (
            <p className="mt-2 text-fine text-muted-foreground/70">{card.source}</p>
          )}
          <button
            type="button"
            onClick={() => onDone(verdict)}
            className={`${buttonText} mt-5 -ml-2`}
          >
            Next card
          </button>
        </div>
      )}
    </>
  );
}

/**
 * SPEC §12 — Ten cards: the ten due soonest across the classes, one at a
 * time on a card in its class's wash.
 *
 * A card with options is answered by picking one, and the app marks it. A
 * card that has none yet falls back to the older face — the back on a click,
 * then `Right` or `Wrong` — so the deck keeps working while the options are
 * being written, and a card no fair option set could be made for stays
 * answerable.
 *
 * Either way, right moves the card up a box and wrong sends it to box one for
 * tomorrow. The day's ten are the list as it was fetched; an answered card
 * leaves the face, and the count says how far along the day is.
 */
export function TenCards() {
  const { data: cards, error } = useQuery(dueCardsQuery());
  // The cards answered on this dashboard, by id — the list is not refetched
  // on an answer, so the day's ten hold still while the reader works
  // through them.
  const [answered, setAnswered] = useState<ReadonlyMap<number, boolean>>(() => new Map());
  const [shown, setShown] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  // What the last answer did, in the words the button used. The card the
  // reader judged is gone by the time they can look for it, so without this
  // the only sign a click landed was a different card being there — which
  // reads as nothing having happened at all.
  const [receipt, setReceipt] = useState<{
    right: boolean;
    box: number;
    back: string;
  } | null>(null);
  if (error) {
    return <p className={`${errorLine} mt-12`}>The cards didn't load: {String(error)}</p>;
  }
  if (cards === undefined) return null;

  // Counted off the answers, not the list: a refetch on focus may bring the
  // next due card in behind an answered one, and the day's count must not
  // move with it.
  const left = cards.filter((c) => !answered.has(c.id));
  const card = left[0] ?? null;
  const done = answered.size;
  const total = left.length + done;
  const right = [...answered.values()].filter(Boolean).length;
  const answer = (id: number, wasRight: boolean) => {
    setBusy(true);
    setRefused(null);
    answerCard(id, wasRight)
      .then((scheduled) => {
        setAnswered((prev) => new Map(prev).set(id, wasRight));
        setReceipt({ right: wasRight, box: scheduled.box, back: backIn(scheduled.dueOn) });
        setShown(null);
        setBusy(false);
      })
      .catch((e) => {
        setRefused(String(e));
        setBusy(false);
      });
  };

  return (
    <section className="mt-12" aria-label="Ten cards">
      <SectionHeading
        title="Ten cards"
        count={total === 0 ? undefined : `${done} of ${total}`}
      >
        {/* One live region, so the announcement is made by a child arriving
            inside a parent that stays — a region that mounts with its own
            text is not reliably read out. */}
        <div aria-live="polite">
          {refused !== null && <p className={errorLine}>Not recorded: {refused}</p>}
          {/* Keyed on the count, so a second `Right` in a row fades in again
              rather than sitting there looking like the first one. Dropped on
              the last card, where the day's tally says the same thing. */}
          {refused === null && receipt !== null && card !== null && (
            <p
              key={done}
              className={`mt-1.5 ${meta} animate-in fade-in duration-150 motion-reduce:animate-none`}
            >
              {receipt.right ? "Right" : "Wrong"} · box {receipt.box} of 3 ·{" "}
              {receipt.back}
            </p>
          )}
        </div>
      </SectionHeading>
      {card === null ? (
        <p className={`mt-3 ${readingText} text-muted-foreground`}>
          {total === 0
            ? "No cards due today."
            : `Done for today · ${right} right, ${done - right} wrong. The ones missed come back tomorrow.`}
        </p>
      ) : (
        // Keyed on the card, so answering one remounts the face and the next
        // rises into place instead of the text simply being different. The
        // motion is the app's own (SPEC §12): it answers the click and stops
        // under reduced motion.
        <article
          key={card.id}
          style={{ "--accent": CLASS_ACCENTS[card.classColor] ?? "var(--class-blue)" } as CSSProperties}
          className={`${washCard} mt-4 animate-in fade-in slide-in-from-bottom-2 duration-200 motion-reduce:animate-none`}
          aria-label={`Card ${done + 1} of ${total}`}
        >
          <p className={`flex flex-wrap items-baseline gap-x-2 ${meta}`}>
            <span className="font-medium text-(--accent-ink)">{card.className}</span>
            <span className="min-w-0 truncate">· {card.scopeLabel}</span>
            {card.topic && <span className="min-w-0 truncate">· {card.topic}</span>}
            <span className="ml-auto shrink-0">box {card.box} of 3</span>
          </p>
          <p className={`mt-4 max-w-2xl ${readingText}`}>{card.front}</p>
          {card.choices !== null ? (
            <Choices
              key={card.id}
              card={card}
              onFailed={setRefused}
              onDone={(verdict) => {
                setAnswered((prev) => new Map(prev).set(card.id, verdict.right));
                setReceipt({
                  right: verdict.right,
                  box: verdict.card.box,
                  back: backIn(verdict.card.dueOn),
                });
              }}
            />
          ) : shown === card.id ? (
            <>
              <p className={`mt-4 max-w-2xl border-t border-(--accent)/25 pt-4 ${readingText} text-muted-foreground`}>
                {card.back}
              </p>
              {card.source && (
                <p className="mt-2 text-fine text-muted-foreground/70">{card.source}</p>
              )}
              {/* Four pixels apart, these were one slip from recording the
                  opposite of what was meant, on a control pressed ten times a
                  day and never confirmed. */}
              <div className="mt-5 -ml-2 flex items-center gap-4">
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => answer(card.id, true)}
                  className={buttonText}
                >
                  Right
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => answer(card.id, false)}
                  className={buttonTextMuted}
                >
                  Wrong
                </button>
              </div>
            </>
          ) : (
            <button
              type="button"
              onClick={() => setShown(card.id)}
              className={`${buttonText} mt-5 -ml-2`}
            >
              Show the back
            </button>
          )}
        </article>
      )}
    </section>
  );
}
