/** "reading sources" → "Reading sources": the first letter and nothing else. */
export function sentence(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
