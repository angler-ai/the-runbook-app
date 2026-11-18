// CF11: what grows in D1, measured. Drives a realistic project through the
// built api Worker under local workerd (tests/harness.mjs; LOCAL ONLY):
// runbooks of 4–50 KB with drafts saved and versions published, skills,
// a webhook subscription, runs started and stepped. Then it reports, per
// D1 table, the rows and their stored bytes (the sum of each column's
// length; integers count their decimal digits, so the figure is a close
// lower bound, not SQLite's page usage).
//
//   node scripts/d1-sizes.mjs [--json out.json]
import { randomBytes } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { startWorker } from '../tests/harness.mjs';

const RUNBOOK_KB = [4, 8, 12, 20, 30, 50];
const RUNS_PER_RUNBOOK = 5;
const SECRET = randomBytes(24).toString('hex');

/** A valid runbook of about `kb` KB: human steps with instructions and
 * checklists, assigned to `user` (the shape of real operational runbooks:
 * mostly prose). */
export function runbookSource(id, kb, user, edit = 0) {
  const lines = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata:',
    `  id: ${id}`,
    `  title: Operate ${id} (revision ${edit})`,
    '  description: Checks, decides and records each stage of a production change.',
    '  tags: [ops, production]',
    'spec:',
    '  steps:',
  ];
  const paragraph =
    'Confirm the dashboards for the service show the expected traffic, compare the error budget ' +
    'burn against the last seven days, and write down anything unusual with a link to the graph. ' +
    'If a check fails, stop and hand the step back with what you saw rather than retrying blindly.';
  let step = 0;
  while (lines.join('\n').length < kb * 1024) {
    step += 1;
    lines.push(
      `    - id: step_${step}`,
      `      title: Stage ${step} of the change`,
      `      assignee: "user:${user}"`,
      '      instructions: |',
      `        ${paragraph}`,
      `        Stage ${step}: ${paragraph}`,
      '      checklist: [Dashboards checked, Error budget compared, Notes written]',
    );
  }
  return `${lines.join('\n')}\n`;
}

/** Rows and stored bytes per table of `db`. */
export async function measure(db) {
  const tables = await db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY name")
    .all();
  const out = [];
  for (const { name } of tables.results) {
    const columns = (await db.prepare(`PRAGMA table_info("${name}")`).all()).results.map((c) => c.name);
    if (!columns.length) continue;
    const size = columns.map((c) => `coalesce(length(CAST("${c}" AS BLOB)), 0)`).join(' + ');
    const row = await db
      .prepare(`SELECT count(*) AS n, coalesce(sum(${size}), 0) AS total, coalesce(max(${size}), 0) AS max FROM "${name}"`)
      .first();
    if (row.n === 0) continue;
    out.push({ table: name, rows: row.n, bytes: row.total, avg: Math.round(row.total / row.n), max: row.max });
  }
  return out.sort((a, b) => b.bytes - a.bytes);
}

export function table(rows) {
  const lines = ['| table | rows | bytes | avg/row | max/row |', '|---|---:|---:|---:|---:|'];
  for (const r of rows) lines.push(`| ${r.table} | ${r.rows} | ${r.bytes} | ${r.avg} | ${r.max} |`);
  return lines.join('\n');
}

async function main() {
  const worker = await startWorker({
    faults: false,
    vars: {
      BOOTSTRAP_SECRET: SECRET,
      WEBHOOK_SECRET_KEY: randomBytes(32).toString('base64'),
      WEBHOOKS_ALLOW_PRIVATE: 'on',
      ORIGIN: 'https://runbook.test',
    },
  });
  try {
    const call = async (token, method, path, body, { key, ifMatch } = {}) => {
      const headers = { authorization: `Bearer ${token}` };
      if (method !== 'GET') headers['idempotency-key'] = key ?? randomBytes(9).toString('hex');
      if (ifMatch) headers['if-match'] = ifMatch;
      if (body !== undefined) headers['content-type'] = 'application/json';
      const response = await worker.fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
      const text = await response.text();
      let parsed = null;
      try {
        parsed = JSON.parse(text);
      } catch {}
      if (response.status >= 300) throw new Error(`${method} ${path}: ${response.status} ${text}`);
      return { body: parsed, etag: response.headers.get('etag') };
    };
    const boot = await worker.fetch('/internal/bootstrap', {
      method: 'POST',
      headers: { 'x-bootstrap-secret': SECRET, 'content-type': 'application/json' },
      body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops' }),
    });
    const owner = await boot.json();
    const { personal_token: pat, project_id: project, user_id: user } = owner;
    await call(pat, 'POST', `/v1/projects/${project}/webhooks`, { name: 'sink', url: 'http://127.0.0.1:9/hook', events: ['*'] }, { key: 'hook' });
    const b64 = (s) => Buffer.from(s).toString('base64');
    for (const [i, name] of ['check-error-rates', 'rollback-service'].entries()) {
      await call(pat, 'POST', `/v1/projects/${project}/skills`, {
        files: [
          { path: 'SKILL.md', content: b64(`---\nname: ${name}\ndescription: Operates the service.\n---\n${'Run the procedure carefully. '.repeat(200)}\n`) },
          { path: 'run.sh', content: b64(`#!/bin/sh\n${'echo step\n'.repeat(400)}`) },
          { path: `notes-${i}.md`, content: b64('A note. '.repeat(500)) },
        ],
        reference: `skills-${i}`,
      });
    }
    let runs = 0;
    for (const kb of RUNBOOK_KB) {
      const id = `ops-${kb}kb`;
      const created = await call(pat, 'POST', `/v1/projects/${project}/runbooks`, { source: runbookSource(id, kb, user) }, { key: `create-${id}` });
      const rb = created.body.runbook_id;
      for (let edit = 1; edit <= 3; edit += 1) {
        const draft = await call(pat, 'GET', `/v1/runbooks/${rb}/draft`);
        await call(pat, 'POST', `/v1/runbooks/${rb}/versions`, {}, { key: `publish-${id}-${edit}`, ifMatch: draft.etag });
        const again = await call(pat, 'GET', `/v1/runbooks/${rb}/draft`);
        await call(pat, 'PATCH', `/v1/runbooks/${rb}/draft`, { source: runbookSource(id, kb, user, edit) }, { ifMatch: again.etag });
      }
      for (let n = 0; n < RUNS_PER_RUNBOOK; n += 1) {
        const started = await call(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: id }, { key: `start-${id}-${n}` });
        runs += 1;
        if (n % 2) continue;
        // Step the first two steps of every other run.
        for (let s = 0; s < 2; s += 1) {
          let step;
          for (let tries = 0; tries < 50 && !step; tries += 1) {
            const read = await call(pat, 'GET', `/v1/executions/${started.body.id}`);
            step = read.body.steps.find((x) => x.status === 'ready' || x.status === 'pending' || x.status === 'waiting')?.id;
            if (!step) await new Promise((r) => setTimeout(r, 50));
          }
          if (!step) break;
          const claim = await call(pat, 'POST', `/v1/step-runs/${step}/claim`, { session_id: 'measure' }, { key: `claim-${step}` });
          await call(pat, 'POST', `/v1/step-runs/${step}/checklist`, {
            claim: claim.body,
            checklist: ['Dashboards checked', 'Error budget compared', 'Notes written'].map((title) => ({ title, completed: true })),
          });
          await call(pat, 'POST', `/v1/step-runs/${step}/report`, {
            step_run_id: step,
            claim: claim.body,
            status: 'succeeded',
            verdict: null,
            outputs: {},
            summary: 'Checked: dashboards normal, error budget within range.',
            evidence: [],
            logs: [],
          }, { key: `report-${step}` });
        }
      }
    }
    const handler = await worker.mf.getWorker();
    await handler.scheduled({ cron: '* * * * *', scheduledTime: new Date() });
    const db = await worker.mf.getD1Database('DB');
    const rows = await measure(db);
    const total = rows.reduce((n, r) => n + r.bytes, 0);
    console.log(`workload: ${RUNBOOK_KB.length} runbooks (${RUNBOOK_KB.join(', ')} KB), 3 versions and 4 drafts each, 2 skills, ${runs} runs`);
    console.log(table(rows));
    console.log(`\ntotal: ${total} bytes`);
    const at = process.argv.indexOf('--json');
    if (at > 0) await writeFile(process.argv[at + 1], JSON.stringify({ runs, rows, total }, null, 2));
  } finally {
    await worker.dispose();
  }
}

if (import.meta.url === `file://${process.argv[1]}`) await main();
