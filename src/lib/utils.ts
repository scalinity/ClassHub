import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/** "reading sources" → "Reading sources": the first letter and nothing else. */
export function sentence(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
