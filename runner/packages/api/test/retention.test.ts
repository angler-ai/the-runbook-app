import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gunzipSync } from 'node:zlib';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Config } from '@runbook/runner-shared/config';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { createTestDatabase, hasTestDatabase, type TestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { archiveKey, runRetention, sha256 } from '../src/retention/job.ts';
import { retentionConfig } from '../src/retention/config.ts';
import { archiveStore, type ArchiveStore } from '../src/retention/storage.ts';
import { publicResources } from '../src/resources/public.ts';
import { loadUsageMetrics, readUsage, usageWindow } from '../src/resources/usage.ts';
import { budgetsOverview } from '../src/ledger/overview.ts';
import { reserve } from '../src/ledger/reserve.ts';
import { settle } from '../src/ledger/settle.ts';

const org = 'org';
const now = new Date('2026-11-23T00:00:00Z');
const old = new Date('2026-10-23T00:00:00Z');
const tomorrow = new Date(+now + 86_400_000);
const config = retentionConfig({});
const user: UserAssertionClaims = {
  iss: 'core', aud: 'runner:install', jti: 'test', iat: 0, exp: 60, user_id: 'reader',
  org_id: org, org_role: 'owner', principal: 'human', scopes: ['runner:read'], project_roles: { project: 'viewer' },
  htm: 'GET', htu: '/runner/v1/budgets', bh: '',
};
const transcript = '/runner/v1/transcripts/step';
const tools = '/runner/v1/projects/project/agents/agent/tool-calls?step_run_id=step';

it('defaults to 30 live days and keeps archives indefinitely', () => {
  expect(config).toMatchObject({ liveDays: 30, archiveDays: null });
  expect(retentionConfig({ RUNNER_RETENTION_LIVE_DAYS: 'off' })).toMatchObject({ liveDays: null, archiveDays: null });
  expect(retentionConfig({ RUNNER_RETENTION_ARCHIVE_DAYS: '30' }).archiveDays).toBe(30);
});

it.each([
  ['30', '29', /RUNNER_RETENTION_ARCHIVE_DAYS must be off or at least/],
  ['off', '30', /RUNNER_RETENTION_ARCHIVE_DAYS must be off or at least/],
  ['0', 'off', /RUNNER_RETENTION_LIVE_DAYS must be a whole number/],
  ['1.5', 'off', /RUNNER_RETENTION_LIVE_DAYS must be a whole number/],
  ['30', '36501', /RUNNER_RETENTION_ARCHIVE_DAYS must be a whole number/],
] as const)('rejects live %s and archive %s', (live, archive, error) => {
  expect(() => retentionConfig({ RUNNER_RETENTION_LIVE_DAYS: live, RUNNER_RETENTION_ARCHIVE_DAYS: archive })).toThrow(error);
});

describe.skipIf(!hasTestDatabase)('runner retention (Postgres)', () => {
  let db: TestDatabase;
  let dir: string;
  let store: ArchiveStore;
  beforeEach(async () => {
    db = await createTestDatabase();
    dir = await mkdtemp(join(tmpdir(), 'runner-retention-'));
    store = archiveStore(dir);
  });
  afterEach(async () => {
    await db?.drop();
    if (dir) await rm(dir, { recursive: true, force: true });
  });

  async function addTranscript(step = 'step', at = old, project = 'project') {
    await db.admin().query(`
      INSERT INTO transcripts (step_run_id, group_id, runner_id, project_id, agent_id, entry, claim_id, generation, created_at)
      VALUES ($1, 'group', 'runner', $2, 'agent', '{"message":"redacted entry"}', 'claim', 2, $3)
    `, [step, project, at]);
  }

  async function addTool() {
    await db.admin().query(`
      INSERT INTO tool_calls (step_run_id, group_id, runner_id, project_id, agent_id, server_id, tool, created_at)
      VALUES ('step', 'group', 'runner', 'project', 'agent', 'server', 'search', $1)
    `, [old]);
  }

  function routes(readerStore = store) {
    const handle = publicResources(db.pool('api'), {} as Config, async () => {}, undefined,
      { store: readerStore, orgId: org, archiveDays: null });
    return (target: string, claims = user) => handle({ method: 'GET', target, claims, body: undefined });
  }

  it('archives all five detail tables into one verified daily file, then deletes hot rows and preserves audit', async () => {
    const admin = db.admin();
    await addTranscript();
    await addTranscript('recent', new Date('2026-11-01T00:00:00Z'));
    await addTool();
    await admin.query(`
      INSERT INTO releases (id, step_run_id, credential_id, field, credential_version, delivery, status, expires_at, created_at)
      VALUES ('release', 'step', 'credential', 'token', 1, 'proxy', 'expired', $1, $1)
    `, [old]);
    await admin.query(`
      INSERT INTO ledger_reservations (id, step_run_id, group_id, runner_id, bucket_ids, kind, amount_usd, status, settled_at, created_at)
      VALUES ('hold', 'step', 'group', 'runner', '[]', 'model', '1', 'settled', $1, $1)
    `, [old]);
    await admin.query(`
      INSERT INTO usage_events (step_run_id, group_id, runner_id, project_id, agent_id, kind, reservation_id, created_at)
      VALUES ('step', 'group', 'runner', 'project', 'agent', 'model', 'hold', $1)
    `, [old]);
    await admin.query("INSERT INTO audit_log (id, actor, action, resource_type) VALUES ('audit', '{}', 'test', 'step')");
    const audit = (await admin.query('SELECT seq, hash FROM audit_log')).rows;
    expect(await runRetention(db.pool('api'), store, org, config, now)).toBe(1);
    const { rows: [index] } = await admin.query<{ day: string; sha256: string }>('SELECT day::text, sha256 FROM runner_archive_days');
    const bytes = await readFile(join(dir, archiveKey(org, index!.day, index!.sha256)));
    expect(sha256(bytes)).toBe(index!.sha256);
    const [manifest, detail] = gunzipSync(bytes).toString().trim().split('\n').map((line) => JSON.parse(line));
    expect(manifest).toEqual({ format: 1, org_id: org, day: '2026-11-23', step_count: 1 });
    for (const table of ['transcripts', 'tool_calls', 'releases', 'usage_events', 'ledger_reservations']) {
      expect(detail[table]).toHaveLength(1);
      expect((await admin.query(`SELECT 1 FROM ${table} WHERE step_run_id = 'step'`)).rowCount).toBe(0);
    }
    expect((await admin.query("SELECT 1 FROM transcripts WHERE step_run_id='recent'")).rowCount).toBe(1);
    expect((await admin.query('SELECT seq, hash FROM audit_log')).rows).toEqual(audit);
  });

  it('retries a crash after publish across midnight without losing or duplicating detail', async () => {
    await addTranscript();
    const crash = { ...store, get: async () => { throw new Error('lost connection after publish'); } };
    const pool = db.pool('api');
    await expect(runRetention(pool, crash, org, config, now)).rejects.toThrow('lost connection after publish');
    const admin = db.admin();
    expect((await admin.query('SELECT 1 FROM transcripts')).rowCount).toBe(1);
    expect((await admin.query('SELECT 1 FROM runner_archive_steps')).rowCount).toBe(0);
    expect(await runRetention(pool, store, org, config, tomorrow)).toBe(1);
    expect(await runRetention(pool, store, org, config, tomorrow)).toBe(0);
    const files = await readdir(join(dir, sha256(org)));
    expect(files).toHaveLength(1);
    expect(files[0]).toMatch(/^2026-11-23-/);
    expect((await admin.query('SELECT 1 FROM transcripts')).rowCount).toBe(0);
    expect((await admin.query('SELECT 1 FROM runner_archive_steps')).rowCount).toBe(1);
  });

  it('keeps unsettled and reconciliation-pending holds and live slots in Postgres', async () => {
    const admin = db.admin();
    for (const step of ['reserved', 'pending', 'running']) await addTranscript(step);
    await admin.query(`
      INSERT INTO ledger_reservations (id, step_run_id, group_id, runner_id, bucket_ids, kind,
        amount_usd, status, usage_pending, created_at)
      VALUES ('hold', 'reserved', 'group', 'runner', '[]', 'model', '1', 'reserved', false, $1),
        ('pending', 'pending', 'group', 'runner', '[]', 'model', '1', 'settled', true, $1)
    `, [old]);
    await admin.query(`
      INSERT INTO slots(id, step_run_id, group_id, runner_id, project_id, agent_id, created_at)
      VALUES ('active', 'running', 'group', 'runner', 'project', 'agent', $1)
    `, [old]);
    expect(await runRetention(db.pool('api'), store, org, config, now)).toBe(0);
    expect((await admin.query('SELECT 1 FROM transcripts')).rowCount).toBe(3);
    expect((await admin.query('SELECT 1 FROM ledger_reservations')).rowCount).toBe(2);
  });

  it('preserves daily sparklines, usage totals, metrics and enforcement of the current month cap', async () => {
    const admin = db.admin();
    const scheduler = db.pool('scheduler');
    const api = db.pool('api');
    const ctx = { orgId: org, groupId: 'group', projectId: 'project', agentId: 'agent', runnerId: 'runner',
      runId: 'run', stepId: 'step', timeZone: 'UTC', caps: { 'org:month': 1, step: 1, run: 1 } };
    await admin.query('UPDATE policy SET org_cost_per_month_usd = 1');
    const charge = await reserve(scheduler, ctx, 0.75, 'model');
    const admission = await reserve(scheduler, ctx, 0.1, 'admission');
    if (!('reservation_id' in charge) || !('reservation_id' in admission)) throw new Error('fixture reservation refused');
    await admin.query(`
      INSERT INTO slots(id, step_run_id, execution_id, group_id, runner_id, project_id, agent_id, released_at, admission_reservation_id)
      VALUES ('slot', 'step', 'run', 'group', 'runner', 'project', 'agent', now(), $1)
    `, [admission.reservation_id]);
    await admin.query("UPDATE ledger_reservations SET slot_id='slot' WHERE id=$1", [charge.reservation_id]);
    await settle(scheduler, { reservation_id: charge.reservation_id, actual_usd: 0.75, usage: { input_tokens: 123 } });
    await settle(scheduler, { reservation_id: admission.reservation_id, actual_usd: 0, usage: {} });
    const window = usageWindow('project', 'month', new Date().toISOString().slice(0, 10));
    const budgets = await budgetsOverview(api, user);
    const usage = await readUsage(api, window, ['project']);
    const metrics = await loadUsageMetrics(api);
    expect(budgets.items).toEqual(expect.arrayContaining(['org', 'run', 'step'].map((scope) =>
      expect.objectContaining({ scope, spend: 0.75, daily_spend: expect.arrayContaining([expect.objectContaining({ spend_usd: 0.75 })]) }),
    )));
    expect(await runRetention(api, store, org, config, new Date(Date.now() + 32 * 86_400_000))).toBe(1);
    expect(await budgetsOverview(api, user)).toEqual(budgets);
    expect(await readUsage(api, window, ['project'])).toEqual(usage);
    expect(await loadUsageMetrics(api)).toBe(metrics);
    expect((await admin.query("SELECT admission_reservation_id FROM slots WHERE id='slot'")).rows).toEqual([{ admission_reservation_id: null }]);
    expect(await reserve(scheduler, { ...ctx, stepId: 'later', runId: 'later-run' }, 0.26, 'model')).toEqual({ level: 'org' });
  });

  it('serves the same transcript and tool fields with archive metadata and honors transcript filters', async () => {
    await addTranscript();
    await addTool();
    const get = routes();
    const hotTranscript = (await get(transcript)).body as { items: Record<string, unknown>[] };
    const hotTools = (await get(tools)).body as { items: Record<string, unknown>[] };
    await runRetention(db.pool('api'), store, org, config, now);
    const metadata = { archived: true, archived_at: '2026-11-23' };
    expect((await get(transcript)).body).toEqual({ ...metadata, items: hotTranscript.items.map((row) => ({
      ...row, created_at: (row.created_at as Date).toISOString(), ...metadata,
    })) });
    expect((await get(tools)).body).toEqual({ ...metadata, items: hotTools.items.map((row) => ({
      ...row, created_at: (row.created_at as Date).toISOString(), ...metadata,
    })) });
    expect((await get(transcript + '?claim_id=other')).body).toEqual({ items: [] });
    expect((await get(transcript + '?after=1')).body).toEqual({ items: [] });
  });

  it('does not fetch archived detail for callers without runner:read or project access', async () => {
    await addTranscript();
    await addTool();
    await runRetention(db.pool('api'), store, org, config, now);
    const watch = vi.fn(store.get);
    const get = routes({ ...store, get: watch });
    for (const endpoint of [transcript, tools]) {
      await expect(get(endpoint, { ...user, scopes: [] })).rejects.toMatchObject({ status: 403 });
    }
    await expect(get(tools, { ...user, project_roles: {} })).rejects.toMatchObject({ status: 403 });
    expect((await get(transcript, { ...user, project_roles: {} })).body).toEqual({ items: [] });
    expect((await get(transcript, { ...user, project_roles: { other: 'viewer' } })).body).toEqual({ items: [] });
    expect(watch).not.toHaveBeenCalled();
  });

  it('revalidates live writes before removal and lets another leader skip the in-progress job', async () => {
    await addTranscript();
    const pool = db.pool('api');
    const concurrent = { ...store, put: async (key: string, bytes: Buffer) => {
      expect(await runRetention(pool, store, org, config, now)).toBe(0);
      await store.put(key, bytes);
      await addTranscript('step', now);
    } };
    await expect(runRetention(pool, concurrent, org, config, now)).rejects.toThrow('detail changed');
    expect(await runRetention(pool, store, org, config, now)).toBe(0);
    expect((await db.admin().query('SELECT 1 FROM transcripts')).rowCount).toBe(2);
    expect(await readdir(join(dir, sha256(org)))).toHaveLength(0);
  });

  it('expires archive objects and their read index after the archive window', async () => {
    await addTranscript();
    const pool = db.pool('api');
    const config = retentionConfig({ RUNNER_RETENTION_ARCHIVE_DAYS: '30' });
    await runRetention(pool, store, org, config, now);
    await runRetention(pool, store, org, config, new Date(+now + 30 * 86_400_000));
    expect(await readdir(join(dir, sha256(org)))).toHaveLength(0);
    expect((await routes()(transcript)).body).toEqual({ items: [] });
  });
});
