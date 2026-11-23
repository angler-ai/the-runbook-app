/** Exponential delay with jitter in the upper half of the window. */
export function retryDelay(attempt: number, baseMs: number, maxMs: number): number {
  const ceiling = Math.min(maxMs, baseMs * 2 ** attempt);
  return Math.floor(ceiling * (0.5 + Math.random() * 0.5));
}
