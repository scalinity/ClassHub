/**
 * Shared control surfaces from the SPEC §12 design language.
 *
 * Two focus rings, not one: `--accent` is the class colour, and it only exists
 * inside a class-scoped container. Screens that sit outside one (Settings, the
 * chat settings pane) take the neutral `ring` instead — that is the reason the
 * two variants differ, so reach for the `Neutral` name deliberately rather than
 * letting a copy drift onto the wrong ring.
 */

/** Mono label button on a class-scoped surface. Caller supplies the colour. */
export const monoAction =
  "shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] transition-colors focus-visible:outline-2 focus-visible:outline-(--accent)";

/** `monoAction` for screens with no class accent in scope. */
export const monoActionNeutral =
  "shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] transition-colors focus-visible:outline-2 focus-visible:outline-ring";

/** Mono label button in a document viewer header — carries its own muted ink. */
export const headerAction =
  "shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)";

/** Bare icon button, sidebar scale. */
export const iconAction =
  "shrink-0 cursor-pointer rounded p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring";

/** Bare icon button, inline-in-a-row scale, on a class-scoped surface. */
export const iconActionAccent =
  "cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted focus-visible:outline-2 focus-visible:outline-(--accent)";

/** Single-line text/number/date field. */
export const inputBase =
  "h-8 rounded-md border bg-transparent px-2.5 text-[13px] focus-visible:outline-2 focus-visible:outline-(--accent)";
