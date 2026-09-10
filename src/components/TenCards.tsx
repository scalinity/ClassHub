import { useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";

import { SectionHeading } from "@/components/SectionHeading";
import { answerCard, dueCardsQuery } from "@/lib/cards";
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
 * SPEC §12 — Ten cards: the ten due soonest across the classes, one at a
 * time on a card in its class's wash — the front, then the back on a click,
 * then `Right` or `Wrong`. Right moves the card up a box, wrong sends it to
 * box one for tomorrow. The day's ten are the list as it was fetched; a
 * card answered leaves the face, and the count says how far along the day
 * is.
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
          {shown === card.id ? (
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
