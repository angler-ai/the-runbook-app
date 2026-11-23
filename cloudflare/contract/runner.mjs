// Local Postgres entry point for the runner scenario also run under workerd.
// Run with DATABASE_URL and the built runbook-server, like contract/run.mjs.
import { createServer } from 'node:http';
import { once } from 'node:events';
import { runnerClient, runnerScenario } from '../../crates/runbook-worker-conformance/cases/runner.mjs';
import { startNative } from './targets.mjs';

// Match the default GKE core pool size.
process.env.RUNBOOK_DATABASE_POOL_SIZE = '5';
const forwarded = [];
let runnerReply = () => Response.json({ groups: [] }, { headers: { 'set-cookie': 'runner=secret' } });
const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  forwarded.push({ url, method: req.method, headers: new Headers(req.headers), body: Buffer.concat(chunks).toString() });
  const reply = await runnerReply();
  const answer = typeof reply === 'function' ? reply() : reply;
  res.writeHead(answer.status, Object.fromEntries(answer.headers));
  res.end(Buffer.from(await answer.arrayBuffer()));
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
let pg;
try {
  pg = await startNative({ databaseUrl: process.env.DATABASE_URL, runnerPrivateNetworks: true });
  await pg.grantRunnerScopes();
  await runnerScenario({
    call: runnerClient((path, init) => fetch(new URL(path, pg.origin), init)),
    token: pg.pat, project: pg.project, org: pg.org, user: pg.user,
    url: `http://127.0.0.1:${server.address().port}`, forwarded,
    readiness: '/health/ready',
    native: true,
    setRunnerReply: (reply) => { runnerReply = reply; },
  });
  for (const marker of ['synthetic-runner-oauth-code-marker', 'SYNTHETIC_RUNNER_OAUTH_STATE_MARKER']) {
    if (pg.logs().includes(marker)) throw new Error('runner OAuth callback query leaked to the server log');
  }
  console.log('Postgres runner contract passed');
} catch (error) {
  if (pg) console.error(pg.logs());
  throw error;
} finally {
  await pg?.stop();
  await new Promise((done) => server.close(done));
}
