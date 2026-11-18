// Display names for user ids (walkthrough #1). Pages resolve ids through
// the org directory; these are the fallbacks for an id it does not know,
// so a raw 30-character id never fills a line.

/** Resolves a user id to the name to show. */
export type NameOf = (userId: string) => string;

/** `usr_01M76R2QZ86FKFYJG2JX2PWR4P` → `usr_…PWR4P`; other text is returned as is. */
export function shortId(id: string): string {
  const match = /^([a-z]+_)([0-9A-Za-z]{10,})$/.exec(id);
  if (!match) return id;
  return `${match[1]}…${(match[2] ?? '').slice(-5)}`;
}

/** Whether a value is shaped like a user id (`usr_` and a 26-character ULID). */
export function isUserId(value: unknown): value is string {
  return typeof value === 'string' && /^usr_[0-9A-Za-z]{26}$/.test(value);
}
