import type pg from 'pg';

interface HeartbeatOptions {
  coreUrl: string;
  installId: string;
  token: string;
  runnerId: string;
  workerUrl: string;
  capacity: number;
  pool: pg.Pool;
  fetch?: typeof fetch;
}

export async function sendInstallationHeartbeat(options: HeartbeatOptions): Promise<void> {
  const fetcher = options.fetch ?? fetch;
  // A response from the worker's private HTTP server establishes that it
  // is listening without invoking the privileged wake route.
  const ready = await fetcher(options.workerUrl, { signal: AbortSignal.timeout(5000), redirect: 'error' })
    .then(async (response) => { await response.body?.cancel(); return true; }, () => false);
  const query = {
    text: 'SELECT count(*)::int AS running FROM slots WHERE runner_id = $1 AND released_at IS NULL',
    values: [options.runnerId], query_timeout: 5000,
  };
  const { rows: [leases] } = await options.pool.query<{ running: number }>(query);
  const response = await fetcher(`${options.coreUrl}/v1/runner-installs/${encodeURIComponent(options.installId)}/heartbeat`, {
    method: 'POST', headers: { authorization: `Bearer ${options.token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ runners: [{
      id: options.runnerId, status: ready ? 'ready' : 'offline', capacity: options.capacity, running: leases!.running,
    }] }),
    signal: AbortSignal.timeout(5000), redirect: 'error',
  });
  await response.body?.cancel();
  if (!response.ok) throw new Error('core refused runner heartbeat');
}

export function startInstallationHeartbeat(options: HeartbeatOptions, onError: () => void): () => void {
  let stopped = false;
  let timer: NodeJS.Timeout | undefined;
  const heartbeat = () => {
    void sendInstallationHeartbeat(options).catch(onError).finally(() => {
      if (!stopped) timer = setTimeout(heartbeat, 30_000).unref();
    });
  };
  heartbeat();
  return () => { stopped = true; clearTimeout(timer); };
}
