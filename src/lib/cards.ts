import { invoke } from "@tauri-apps/api/core";

import { todayIso } from "@/lib/schedule";

/**
 * SPEC §12 — the cards every guide and digest writes beside itself, indexed
 * on read, served ten a day on the dashboard and exported for Anki.
 */

export interface CardInfo {
  id: number;
  classId: number;
  className: string;
  classColor: string;
  scope: string;
  /** What the scope is called on screen — the guide's or the session's name. */
  scopeLabel: string;
  front: string;
  back: string;
  source: string | null;
  topic: string | null;
  /** 1–3: right moves a card up, wrong sends it back to 1. */
  box: number;
  /** YYYY-MM-DD; null until first shown, which is due now. */
  dueOn: string | null;
}

/** The ten cards due soonest across the classes. */
export function dueCards(): Promise<CardInfo[]> {
  return invoke<CardInfo[]>("due_cards", { today: todayIso() });
}

export function dueCardsQuery() {
  return {
    queryKey: ["cards", "due", todayIso()] as const,
    queryFn: dueCards,
  };
}

/** `Right` or `Wrong` on a card; answers the card as it is now. */
export function answerCard(id: number, right: boolean): Promise<CardInfo> {
  return invoke<CardInfo>("answer_card", { id, right, today: todayIso() });
}

/** A class's cards, indexed from its sidecars on the way. */
export function listCards(classId: number): Promise<CardInfo[]> {
  return invoke<CardInfo[]>("list_cards", { classId });
}

/** `Export for Anki`: writes the class's cards as one tab-separated file
 *  and answers its class-relative path. */
export function exportCards(classId: number): Promise<string> {
  return invoke<string>("export_cards", { classId });
}
