import type { ReactNode } from "react";

import { meta } from "@/lib/styles";

/**
 * A section's first row (SPEC §12): the serif title, its count as meta beside
 * it, text actions far right. No rule under it — the rows below are separated
 * by hairlines because they are a list.
 */
export function SectionHeading({
  title,
  count,
  actions,
  children,
}: {
  title: string;
  /** Already a phrase: "11 open", "2 from Canvas", "13 files". */
  count?: string;
  /** Text buttons and status lines, right-aligned. */
  actions?: ReactNode;
  /** A line under the heading — an error, a provenance note. */
  children?: ReactNode;
}) {
  return (
    <>
      <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
        <h2 className="text-headline">{title}</h2>
        {count !== undefined && <span className={meta}>{count}</span>}
        {actions !== undefined && (
          <div className="ml-auto flex flex-wrap items-center gap-1">{actions}</div>
        )}
      </div>
      {children}
    </>
  );
}
