import { appendFile, mkdir, readFile, rmdir } from 'node:fs/promises';
import { dirname } from 'node:path';

export interface SpendEntry {
  timestamp: string;
  subject: string;
  task: string;
  runDir: string;
  trialId?: string;
  modelCostUsd: number;
  judgeCostUsd: number;
  /** A reservation holds spend for an in-flight trial until an entry with the same reservationId settles it. */
  reservation?: boolean;
  reservationId?: string;
}

export async function readSpendMicros(path: string) {
  const content = await readFile(path, 'utf8').catch((error: NodeJS.ErrnoException) => {
    if (error.code === 'ENOENT') return '';
    throw error;
  });
  let total = 0;
  // Unsettled reservations count in full, so a killed run's in-flight trial stays charged.
  const open = new Map<string, number>();
  for (const [line, text] of content.split('\n').entries()) {
    if (!text.trim()) continue;
    try {
      const entry = JSON.parse(text) as SpendEntry;
      const { modelCostUsd, judgeCostUsd } = entry;
      if (![modelCostUsd, judgeCostUsd].every((cost) => Number.isFinite(cost) && cost >= 0)) throw new Error();
      const micros = Math.ceil((modelCostUsd + judgeCostUsd) * 1_000_000 - 1e-8);
      if (entry.reservation) {
        if (typeof entry.reservationId !== 'string' || open.has(entry.reservationId)) throw new Error();
        open.set(entry.reservationId, micros);
        continue;
      }
      if (entry.reservationId !== undefined) open.delete(entry.reservationId);
      total += micros;
      if (!Number.isSafeInteger(total)) throw new Error();
    } catch {
      throw new Error(`Invalid spend ledger entry at ${path} line ${line + 1}`);
    }
  }
  for (const micros of open.values()) total += micros;
  if (!Number.isSafeInteger(total)) throw new Error(`Invalid spend ledger total at ${path}`);
  return total;
}

/** Short critical section around a ledger read and append. Waits for other runs instead of failing. */
export async function lockLedger(path: string, waitMs = 30_000) {
  await mkdir(dirname(path), { recursive: true });
  const lock = path + '.lock';
  const deadline = Date.now() + waitMs;
  for (;;) {
    const taken = await mkdir(lock).then(() => true, (error: NodeJS.ErrnoException) => {
      if (error.code === 'EEXIST') return false;
      throw error;
    });
    if (taken) return () => rmdir(lock);
    if (Date.now() >= deadline) throw new Error(`Spend ledger is locked: ${lock}`);
    await new Promise((done) => setTimeout(done, 50 + Math.random() * 100));
  }
}

export async function appendSpend(path: string, entry: SpendEntry) {
  await appendFile(path, JSON.stringify(entry) + '\n', { mode: 0o600, flush: true });
}
