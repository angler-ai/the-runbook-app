// `runner migrate`: apply the runner database migrations as the database
// owner (RUNNER_MIGRATE_DATABASE_URL). The process logins (members of rb_api,
// rb_scheduler, rb_worker, rb_vault) are created by the install, not here.
import { migrate } from './db.ts';
import { createLogger } from './log.ts';

export async function main(): Promise<void> {
  const url = process.env.RUNNER_MIGRATE_DATABASE_URL;
  if (!url) throw new Error('RUNNER_MIGRATE_DATABASE_URL is required');
  const applied = await migrate(url);
  createLogger('migrate').info('migrations applied', { applied });
}
