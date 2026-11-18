import { defineConfig, devices } from '@playwright/test';

// MCP2: the OAuth browser test (`npm run e2e:oauth`), once per edition. Each
// project starts its own stack with sign-in on (stacks.ts), so this config
// has no web server and no global setup. The native project needs
// DATABASE_URL; run it under the shared Postgres lock (e2e/oauth/run.ts).
export default defineConfig({
  testDir: '.',
  testMatch: '**/*.oauth.ts',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 240_000,
  expect: { timeout: 15_000 },
  reporter: [['list']],
  use: {
    ...devices['Desktop Chrome'],
    trace: 'off',
    actionTimeout: 15_000,
    navigationTimeout: 30_000,
  },
  projects: [{ name: 'native' }, { name: 'workers' }],
});
