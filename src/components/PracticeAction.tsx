import { monoAction } from "@/lib/styles";

/**
 * SPEC §8.3 — the workspace's way into a practice exam, beside VIEW GUIDE in
 * every guide cluster: a folder row, a division row and the master strip.
 *
 * A token-costing action, so it keeps the register resynthesize uses: muted,
 * and on a row it stays quiet until the row is hovered or focused. The master
 * strip has no row to hover, so its action stands, like everything else on
 * the strip. While the exam is being written the same slot pulses, the way
 * SYNTHESIZING… does for a guide.
 */
export function PracticeAction({
  active,
  standing = false,
  onSelect,
}: {
  /** A practice job for this scope is queued or running. */
  active: boolean;
  /** Always visible rather than revealed on hover. */
  standing?: boolean;
  onSelect: () => void;
}) {
  if (active) {
    return (
      <span className="flex shrink-0 items-center gap-1.5 px-1.5 font-mono text-[10px] tracking-[0.14em] text-(--accent)">
        <span
          aria-hidden
          className="size-1.5 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
        />
        GENERATING EXAM…
      </span>
    );
  }
  return (
    <button
      type="button"
      title="Write a practice exam from this scope's material"
      onClick={onSelect}
      className={`${monoAction} text-muted-foreground hover:bg-(--accent)/12 hover:text-(--accent)${
        standing
          ? ""
          : " opacity-0 transition-opacity focus-visible:opacity-100 group-focus-within:opacity-100 group-hover:opacity-100"
      }`}
    >
      PRACTICE EXAM
    </button>
  );
}
