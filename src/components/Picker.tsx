import { useId, useRef, useState, type KeyboardEvent } from "react";
import { Check, ChevronsUpDown } from "lucide-react";

export interface PickerOption {
  value: string;
  label: string;
}

/**
 * SPEC §12 — a drop-down on the app's own paper. A native `<select>` opens
 * the system's popup, which no stylesheet reaches, so the list is the app's:
 * a control that reads its value, and on click a list on `--surface` with
 * the chosen row checked. Arrows move, Enter picks, Escape closes, a click
 * anywhere else closes. No `useEffect`: the list focuses itself through its
 * ref callback when it mounts, and the state is a pair of numbers.
 */
export function Picker({
  label,
  value,
  options,
  onChange,
  placeholder = "Choose…",
  disabled = false,
  neutral = false,
  className = "",
  variant = "control",
  drop = "down",
  onOpen,
}: {
  label: string;
  value: string;
  options: PickerOption[];
  onChange: (value: string) => void;
  /** What the control reads while no option matches `value`. */
  placeholder?: string;
  disabled?: boolean;
  /** The neutral focus ring, outside a class scope (see styles.ts). */
  neutral?: boolean;
  /** Width and placement of the control itself. */
  className?: string;
  /**
   * `control` is the bordered field a form wants. `quiet` is the same list
   * hung off a line of running text — the chat footer's model and effort,
   * where a field would be a box around a status line.
   */
  variant?: "control" | "quiet";
  /** Which way the list opens; `up` for a control near the bottom edge. */
  drop?: "down" | "up";
  /** Called as the list opens, for a caller that loads its options lazily. */
  onOpen?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const listId = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const selected = options.findIndex((o) => o.value === value);
  const ring = neutral ? "focus-visible:outline-ring" : "focus-visible:outline-(--accent)";
  const face =
    variant === "quiet"
      ? `flex max-w-full cursor-pointer items-center gap-1 rounded-sm px-1 py-0.5 text-left text-fine text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 ${ring} disabled:pointer-events-none disabled:opacity-50`
      : `flex h-8 w-full cursor-pointer items-center justify-between gap-2 rounded-md border border-border bg-surface px-2.5 text-left text-body transition-colors hover:bg-muted/40 focus-visible:outline-2 ${ring} disabled:pointer-events-none disabled:opacity-50`;

  const show = () => {
    setActive(selected === -1 ? 0 : selected);
    setOpen(true);
    onOpen?.();
  };
  // Closing hands focus back to the control, where the keyboard left it.
  const close = () => {
    setOpen(false);
    trigger.current?.focus();
  };
  const pick = (index: number) => {
    const option = options[index];
    close();
    if (option && option.value !== value) onChange(option.value);
  };
  const onKey = (e: KeyboardEvent<HTMLUListElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(options.length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      pick(active);
    } else if (e.key === "Escape" || e.key === "Tab") {
      // Consumed here: the innermost layer takes the dismissal, or the chat
      // panel's own Escape would close the whole panel behind the list.
      e.stopPropagation();
      close();
    } else if (e.key.length === 1 && e.key !== " ") {
      // A typed letter jumps to the next option opening with it, the way
      // the system's list does.
      const letter = e.key.toLowerCase();
      const order = [...options.keys()].map((i) => (active + 1 + i) % options.length);
      const hit = order.find((i) => options[i].label.toLowerCase().startsWith(letter));
      if (hit !== undefined) setActive(hit);
    }
  };

  return (
    <div className={`relative ${className}`}>
      <button
        ref={trigger}
        type="button"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        disabled={disabled}
        // While the list is open the overlay takes the click and closes it,
        // so this only ever opens.
        onClick={show}
        onKeyDown={(e) => {
          if (!open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
            e.preventDefault();
            show();
          }
        }}
        className={face}
      >
        <span className={`min-w-0 truncate ${selected === -1 ? "text-muted-foreground" : ""}`}>
          {selected === -1 ? placeholder : options[selected].label}
        </span>
        <ChevronsUpDown
          size={variant === "quiet" ? 10 : 12}
          aria-hidden
          className="shrink-0 text-muted-foreground/70"
        />
      </button>
      {open && (
        <>
          {/* A click anywhere else closes the list, before it reaches what is under it. */}
          <div aria-hidden className="fixed inset-0 z-20" onMouseDown={() => setOpen(false)} />
          <ul
            id={listId}
            role="listbox"
            aria-label={label}
            aria-activedescendant={`${listId}-${active}`}
            tabIndex={-1}
            ref={(el) => el?.focus()}
            onKeyDown={onKey}
            className={
              "absolute left-0 z-30 max-h-64 min-w-full overflow-y-auto rounded-lg bg-surface p-1 shadow-lg ring-1 ring-border outline-none animate-in fade-in duration-150 motion-reduce:animate-none " +
              (drop === "up"
                ? "bottom-full mb-1 slide-in-from-bottom-1"
                : "top-full mt-1 slide-in-from-top-1")
            }
          >
            {options.map((option, index) => (
              <li
                key={option.value}
                id={`${listId}-${index}`}
                role="option"
                aria-selected={index === selected}
                // The active row stays in view as the arrows move it through
                // a list taller than the box.
                ref={(el) => {
                  if (el && index === active) el.scrollIntoView({ block: "nearest" });
                }}
                onMouseEnter={() => setActive(index)}
                onClick={() => pick(index)}
                className={
                  "flex cursor-pointer items-center gap-2 whitespace-nowrap rounded-md px-2 py-1.5 text-body " +
                  (index === active ? "bg-muted/70" : "") +
                  (index === selected ? " font-medium" : "")
                }
              >
                <span className="flex w-3.5 shrink-0 justify-center">
                  {index === selected && <Check size={12} aria-hidden />}
                </span>
                <span className="min-w-0 truncate">{option.label}</span>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
