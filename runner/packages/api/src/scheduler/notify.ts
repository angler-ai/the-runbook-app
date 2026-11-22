import type pg from 'pg';
import type { Logger } from '@runbook/runner-shared/log';

interface Notifier { url: string; token: string }

export function workerNotifier(pool: pg.Pool, log: Logger, env = process.env) {
  const raw = env.RUNNER_WORKER_NOTIFIERS;
  const notifiers = new Map<string, Notifier>();
  if (raw) {
    try {
      const parsed = JSON.parse(raw) as Record<string, Notifier>;
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error();
      for (const [runner, notifier] of Object.entries(parsed)) {
        const url = new URL(notifier.url);
        if (!/^[A-Za-z0-9_-]{1,128}$/.test(runner) || url.username || url.password || url.search || url.hash ||
            (url.protocol !== 'https:' && !(url.protocol === 'http:' && env.RUNNER_INSECURE_INTERNAL_HTTP === 'true')) ||
            typeof notifier.token !== 'string' || !/^\S{32,}$/.test(notifier.token)) throw new Error();
        notifiers.set(runner, { url: url.href.replace(/\/+$/, ''), token: notifier.token });
      }
    } catch {
      throw new Error('invalid RUNNER_WORKER_NOTIFIERS');
    }
  }
  return async (group: string) => {
    if (!notifiers.size) return;
    const { rows } = await pool.query<{ runner_id: string }>('SELECT runner_id FROM runners WHERE group_id = $1', [group]);
    await Promise.all(rows.map(async ({ runner_id }) => {
      const notifier = notifiers.get(runner_id);
      if (!notifier) return;
      try {
        const response = await fetch(`${notifier.url}/internal/wake`, {
          method: 'POST', headers: { authorization: `Bearer ${notifier.token}` },
          signal: AbortSignal.timeout(3000), redirect: 'error',
        });
        if (!response.ok) throw new Error();
      } catch {
        log.warn('worker wake failed', { runner_id, group_id: group });
      }
    }));
  };
}
