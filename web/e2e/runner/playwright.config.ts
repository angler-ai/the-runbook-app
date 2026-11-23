import { defineConfig, devices } from '@playwright/test';

const baseURL = 'http://127.0.0.1:5186';
export default defineConfig({
  testDir: '.',
  testMatch: 'runner.walk.ts',
  workers: 2,
  timeout: 90_000,
  reporter: [['list']],
  use: { ...devices['Desktop Chrome'], baseURL, trace: 'off' },
  webServer: {
    command: 'npm run dev -- --port 5186',
    cwd: new URL('../..', import.meta.url).pathname,
    url: baseURL,
    timeout: 30_000,
  },
  projects: [
    {
      name: 'desktop-light',
      use: { viewport: { width: 1360, height: 940 }, colorScheme: 'light' },
    },
    { name: 'desktop-dark', use: { viewport: { width: 1360, height: 940 }, colorScheme: 'dark' } },
    {
      name: 'phone-light',
      use: {
        viewport: { width: 390, height: 844 },
        isMobile: true,
        hasTouch: true,
        colorScheme: 'light',
      },
    },
    {
      name: 'phone-dark',
      use: {
        viewport: { width: 390, height: 844 },
        isMobile: true,
        hasTouch: true,
        colorScheme: 'dark',
      },
    },
  ],
});
