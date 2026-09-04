import { buttonTextMuted, pulseDot, statusLine } from "@/lib/styles";

/**
 * SPEC §8.3 — the workspace's way into a practice exam, beside Read guide in
 * every guide cluster: a folder row, a division row and the master strip.
 *
 * A token-costing action, so it keeps the quiet register a rewrite uses, and
 * on a row it stays hidden until the row is hovered or focused. The master
 * strip has no row to hover, so its action stands, like everything else on
 * the strip. While the exam is being written the same slot pulses, the way
 * "Writing the guide…" does for a guide.
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
      <span className={`shrink-0 px-2 ${statusLine}`}>
        <span aria-hidden className={pulseDot} />
        Writing the exam…
      </span>
    );
  }
  return (
    <button
      type="button"
      title="Write a practice exam from this scope's material"
      onClick={onSelect}
      className={`${buttonTextMuted}${
        standing
          ? ""
          : " opacity-0 transition-opacity focus-visible:opacity-100 group-focus-within:opacity-100 group-hover:opacity-100"
      }`}
    >
      Practice exam
    </button>
  );
}
