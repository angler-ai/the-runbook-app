import { copyFile, mkdtemp, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { expect, it } from 'vitest';
import { migrate, MIGRATIONS_DIR } from '@runbook/runner-shared/db';
import { createTestDatabase, hasTestDatabase, TEST_DATABASE_URL } from '@runbook/runner-shared/test-utils/pg';

it.skipIf(!hasTestDatabase)('keeps ambiguous legacy usage pending without choosing the wrong sibling', async () => {
  const old = await mkdtemp(join(tmpdir(), 'runbook-legacy-ledger-'));
  try {
    for (const name of await readdir(MIGRATIONS_DIR)) {
      if (/^\d{4}_.*\.sql$/.test(name) && name < '0020') {
        await copyFile(new URL(name, MIGRATIONS_DIR), join(old, name));
      }
    }
    const db = await createTestDatabase({
      async beforeMigrate(client) {
        const { rows: [current] } = await client.query<{ name: string }>('SELECT current_database() AS name');
        const database = new URL(TEST_DATABASE_URL!);
        database.pathname = current!.name;
        await migrate(database.toString(), pathToFileURL(old + sep));
        await client.query(`
          INSERT INTO ledger_reservations(id,step_run_id,group_id,runner_id,bucket_ids,kind,amount_usd,
            actual_usd,status,usage_pending,settled_at) VALUES
            ('b','tied','g','r','[]','model',1,1,'settled',true,'2026-01-01 UTC'),
            ('a','tied','g','r','[]','model',1,1,'settled',false,'2026-01-01 UTC'),
            ('c','unique','g','r','[]','model',1,1,'settled',true,'2026-01-01 UTC');
          INSERT INTO usage_events(step_run_id,group_id,runner_id,project_id,agent_id,kind,input_tokens,cost_usd,created_at) VALUES
            ('tied','g','r','p','a','model',99,1,'2026-01-01 UTC'),
            ('tied','g','r','p','a','model',42,1,'2026-01-01 UTC'),
            ('unique','g','r','p','a','model',17,1,'2026-01-01 UTC');
        `);
      },
    });
    try {
      expect((await db.admin().query('SELECT reservation_id, input_tokens, cost_usd FROM usage_events ORDER BY id')).rows)
        .toEqual([
          { reservation_id: 'a', input_tokens: '99', cost_usd: '1.000000' },
          { reservation_id: null, input_tokens: '42', cost_usd: '1.000000' },
          { reservation_id: 'c', input_tokens: '17', cost_usd: '1.000000' },
        ]);
      expect((await db.admin().query('SELECT id FROM ledger_reservations WHERE usage_pending ORDER BY id')).rows)
        .toEqual([{ id: 'b' }, { id: 'c' }]);
    } finally {
      await db.drop();
    }
  } finally {
    await rm(old, { recursive: true, force: true });
  }
});
