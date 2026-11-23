// The Docker install's single-worker heartbeat. Runner API owns the
// directory credential and reports live leases from the scheduler database.
import { main } from '/app/packages/api/dist/main.js';
import { DECISION_CONTRACT, DECISION_CONTRACT_HEADER } from '/app/packages/shared/dist/contracts/core.js';
import { openPool } from '/app/packages/shared/dist/db.js';

const {
  CORE_URL, CORE_DIRECTORY_TOKEN, RUNNER_INSTALL_ID,
  RUNNER_ALLOWED_RUNNER_IDS: runner, RUNNER_CAPACITY, RUNNER_SCHEDULER_DATABASE_URL,
} = process.env;
await main();
const database = openPool('scheduler', RUNNER_SCHEDULER_DATABASE_URL);

async function heartbeat() {
  const signal = AbortSignal.timeout(5_000);
  try {
    // The worker returns 404 on this path. Any HTTP response establishes
    // that the server is listening without invoking its privileged wake route.
    const ready = await fetch('http://runner-worker:8094/', { signal }).then(async (response) => {
      await response.body?.cancel();
      return true;
    }, () => false);
    const decisionServices = ['http://runner-worker:8094', 'http://runner-gateway-control:8092', 'http://runner-vault:8091'];
    const capabilities = await Promise.all(decisionServices.map(async (url) => {
      try {
        const response = await fetch(`${url}/health/decision`, { signal, redirect: 'error' });
        await response.body?.cancel();
        return response.status === 204 && response.headers.get(DECISION_CONTRACT_HEADER) === String(DECISION_CONTRACT);
      } catch {
        return false;
      }
    }));
    const { rows: [leases] } = await database.query({
      text: 'SELECT count(*)::int AS running FROM slots WHERE runner_id = $1 AND released_at IS NULL',
      values: [runner], query_timeout: 5_000,
    });
    const response = await fetch(`${CORE_URL.replace(/\/+$/, '')}/v1/runner-installs/${RUNNER_INSTALL_ID}/heartbeat`, {
      method: 'POST',
      headers: { authorization: `Bearer ${CORE_DIRECTORY_TOKEN}`, 'content-type': 'application/json' },
      body: JSON.stringify({
        runners: [{
          id: runner, status: ready ? 'ready' : 'offline', capacity: Number(RUNNER_CAPACITY), running: leases.running,
          ...(ready && capabilities.every(Boolean) && { decision_contract: DECISION_CONTRACT }),
        }],
      }),
      redirect: 'error', signal: AbortSignal.timeout(5_000),
    });
    if (!response.ok) throw new Error('core refused runner heartbeat');
  } catch {
    console.error('Runner installation heartbeat failed');
  } finally {
    if (!stopping) timer = setTimeout(heartbeat, 30_000).unref();
  }
}

let stopping = false;
let timer;
function stop() {
  stopping = true;
  clearTimeout(timer);
  void database.end();
}
process.once('SIGTERM', stop);
process.once('SIGINT', stop);
void heartbeat();
