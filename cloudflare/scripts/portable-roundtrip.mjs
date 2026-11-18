// CF8: the Workers leg of the Postgres -> archive -> local Workers -> archive
// round trip (crates/runbook-store-postgres/tests/portable.rs,
// `postgres_archive_round_trips_through_local_workers`):
//
//   node scripts/portable-roundtrip.mjs <in.zip> <out.zip>
//
// Starts the built api Worker under workerd (Miniflare, fresh local D1, R2
// and Durable Objects, the runs' background work held so nothing changes
// between the two calls), imports <in.zip> through the operator route and
// writes the operator export to <out.zip>. LOCAL ONLY.
import { randomBytes } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { startWorker } from '../tests/harness.mjs';

const [input, output] = process.argv.slice(2);
if (!input || !output) {
  console.error('usage: node scripts/portable-roundtrip.mjs <in.zip> <out.zip>');
  process.exit(2);
}
const secret = randomBytes(24).toString('hex');
const worker = await startWorker({
  faults: false,
  vars: { RUNBOOK_OPERATOR_SECRET: secret, RUNBOOK_TEST_HOLD_WORK: 'on' },
});
try {
  const headers = { 'x-runbook-operator-secret': secret };
  const imported = await worker.fetch('/internal/portable/import', {
    method: 'POST',
    headers: { ...headers, 'content-type': 'application/zip' },
    body: await readFile(input),
  });
  const summary = await imported.text();
  if (imported.status !== 201) throw new Error(`import answered ${imported.status}: ${summary}`);
  const { org_id: org, project_id: project } = JSON.parse(summary);
  const exported = await worker.fetch(`/internal/portable/export?org=${org}&project=${project}`, { headers });
  if (exported.status !== 200) {
    throw new Error(`export answered ${exported.status}: ${await exported.text()}`);
  }
  await writeFile(output, Buffer.from(await exported.arrayBuffer()));
  console.log(`imported and exported ${summary}`);
} finally {
  await worker.dispose();
}
