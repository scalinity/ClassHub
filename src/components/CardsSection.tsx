import { useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { SectionHeading } from "@/components/SectionHeading";
import { exportCards, listCards, type CardInfo } from "@/lib/cards";
import { revealInFinder } from "@/lib/materials";
import { buttonText, buttonTextMuted, errorLine, meta, row } from "@/lib/styles";

/** The one query the section and the workspace's nav share. */
export function cardsQuery(classId: number) {
  return {
    queryKey: ["cards", "list", classId] as const,
    queryFn: () => listCards(classId),
    placeholderData: (prev: CardInfo[] | undefined) => prev,
  };
}

/**
 * SPEC §12 — the class's cards, indexed from the sidecars every guide and
 * digest writes: one row per document with its count, and `Export for
 * Anki`, which writes the tab-separated file Anki imports and offers it in
 * Finder. Absent until a guide or a distilled lecture has written cards.
 */
export function CardsSection({ classId }: { classId: number }) {
  const { data: cards } = useQuery(cardsQuery(classId));
  const [exported, setExported] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (cards === undefined || cards.length === 0) return null;

  const groups = new Map<string, { label: string; count: number }>();
  for (const card of cards) {
    const group = groups.get(card.scope) ?? { label: card.scopeLabel, count: 0 };
    group.count += 1;
    groups.set(card.scope, group);
  }
  const run = () => {
    setBusy(true);
    setError(null);
    exportCards(classId)
      .then((relPath) => {
        setExported(relPath);
        setBusy(false);
      })
      .catch((e) => {
        setError(String(e));
        setBusy(false);
      });
  };

  return (
    <section id="cards" className="mt-14 scroll-mt-20" aria-label="Cards">
      <SectionHeading
        title="Cards"
        count={cards.length === 1 ? "1 card" : `${cards.length} cards`}
        actions={
          <>
            {exported !== null && (
              <button
                type="button"
                onClick={() => void revealInFinder(classId, exported)}
                className={buttonTextMuted}
              >
                Show in Finder
              </button>
            )}
            <button
              type="button"
              disabled={busy}
              title="One tab-separated file Anki imports — front, back, tags; import it once, and again as it grows"
              onClick={run}
              className={buttonText}
            >
              {busy ? "Writing…" : "Export for Anki"}
            </button>
          </>
        }
      >
        {error && <p className={errorLine}>Not exported: {error}</p>}
        {exported !== null && (
          <p className={`mt-3 ${meta}`}>
            Written to {exported} · import it in Anki; a later import adds what is new.
          </p>
        )}
      </SectionHeading>
      <div className="mt-3">
        {[...groups.entries()].map(([scope, group]) => (
          <div key={scope} className={row}>
            <span className="min-w-0 flex-1 truncate text-title">{group.label}</span>
            <span className={`shrink-0 ${meta}`}>
              {group.count === 1 ? "1 card" : `${group.count} cards`}
            </span>
          </div>
        ))}
      </div>
    </section>
  );
}
