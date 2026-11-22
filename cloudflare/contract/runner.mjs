// Local Postgres entry point for the runner scenario also run under workerd.
// Run with DATABASE_URL and the built runbook-server, like contract/run.mjs.
import { createServer } from 'node:http';
import { once } from 'node:events';
import { runnerClient, runnerScenario } from '../../crates/runbook-worker-conformance/cases/runner.mjs';
import { startNative } from './targets.mjs';

const forwarded = [];
let runnerReply = () => Response.json({ groups: [] }, { headers: { 'set-cookie': 'runner=secret' } });
const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  forwarded.push({ url, method: req.method, headers: new Headers(req.headers), body: Buffer.concat(chunks).toString() });
  const answer = runnerReply();
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
    setRunnerReply: (reply) => { runnerReply = reply; },
  });
  console.log('Postgres runner contract passed');
} catch (error) {
  if (pg) console.error(pg.logs());
  throw error;
} finally {
  await pg?.stop();
  await new Promise((done) => server.close(done));
}
