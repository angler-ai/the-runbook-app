import { defineConfig, devices } from '@playwright/test';

// E2E (M6). Run with `npm run e2e`: e2e/harness/run.ts takes the shared
// Postgres lock, starts an owned database, a native runbook-server, the
// pilot seed and Vite, sets E2E_STATE and E2E_BASE_URL, runs Playwright and
// tears everything down. See e2e/README.md. Browsers are not installed by
// `npm ci`; run `npx playwright install chromium` once.
const baseURL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:5173';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // The screenshot walkthrough (walkthrough.spec.ts, @walkthrough) runs only
  // through `npm run e2e:walkthrough`, which sets E2E_WALKTHROUGH=1; the
  // suite (`npm run e2e`, `e2e:container`) leaves it out.
  ...(process.env.E2E_WALKTHROUGH === '1'
    ? { grep: /@walkthrough/ }
    : { grepInvert: /@walkthrough/ }),
  globalSetup: './e2e/global-setup.ts',
  // One seeded org and one server: specs run in order, one at a time.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 90_000,
  expect: { timeout: 15_000 },
  reporter: [['list']],
  use: {
    baseURL,
    ...devices['Desktop Chrome'],
    // Traces record request headers, which carry bearer tokens: opt in
    // locally with E2E_TRACE=1 and never share the result.
    trace: process.env.E2E_TRACE === '1' ? 'retain-on-failure' : 'off',
    screenshot: 'only-on-failure',
    actionTimeout: 15_000,
    navigationTimeout: 30_000,
  },
  projects: [{ name: 'chromium' }],
});
