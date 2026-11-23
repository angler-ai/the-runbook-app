// Runs once before every test file. Postgres tests (RUNNER_TEST_DATABASE_URL)
// share the cluster-wide roles; create them here, once, so test files that
// each make their own database never race on CREATE ROLE.
export default async function setup(): Promise<void> {
  if (!process.env.RUNNER_TEST_DATABASE_URL) {
    console.warn('RUNNER_TEST_DATABASE_URL is not set: the Postgres tests are skipped.');
    return;
  }
  const { ensureTestRoles } = await import('./packages/shared/test-utils/pg.ts');
  await ensureTestRoles();
}
