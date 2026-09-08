import { useState } from "react";

import { buttonText, buttonTextMuted, input, pulseDot, statusLine } from "@/lib/styles";

/**
 * SPEC §8.3 — the workspace's way into a practice exam, beside Read guide in
 * every guide cluster: a folder row, a division row and the master strip.
 *
 * A token-costing action, so it keeps the quiet register a rewrite uses, and
 * on a row it stays hidden until the row is hovered or focused. The master
 * strip has no row to hover, so its action stands, like everything else on
 * the strip. Pressed, it opens in place into a short form — an optional
 * `Focus on…` field and the button that writes — so the focus the chat tool
 * already passes has a home here too; Escape or an empty blur closes it.
 * While the exam is being written the same slot pulses, the way "Writing the
 * guide…" does for a guide.
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
  /** Write the exam, focused on the given topics when any were typed. */
  onSelect: (focus: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const [focus, setFocus] = useState("");

  if (active) {
    return (
      <span className={`shrink-0 px-2 ${statusLine}`}>
        <span aria-hidden className={pulseDot} />
        Writing the exam…
      </span>
    );
  }
  if (open) {
    const submit = () => {
      const trimmed = focus.trim();
      setOpen(false);
      setFocus("");
      onSelect(trimmed === "" ? null : trimmed);
    };
    return (
      <form
        className="flex shrink-0 items-center gap-1"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            setOpen(false);
            setFocus("");
          }
        }}
      >
        <input
          autoFocus
          aria-label="Focus the exam on"
          placeholder="Focus on…"
          value={focus}
          onChange={(e) => setFocus(e.target.value)}
          onBlur={(e) => {
            // Leaving the field for anything but the button closes the form.
            if (!e.currentTarget.form?.contains(e.relatedTarget as Node | null)) {
              setOpen(false);
              setFocus("");
            }
          }}
          className={`${input} h-7 w-44 text-meta`}
        />
        <button type="submit" className={buttonText}>
          Write the exam
        </button>
      </form>
    );
  }
  return (
    <button
      type="button"
      title="Write a practice exam from this scope's material"
      onClick={() => setOpen(true)}
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
