import { defineConfig } from 'vitest/config';

// Workspace packages resolve to their TypeScript sources through the
// `development` export condition, so tests never need a build first.
export default defineConfig({
  resolve: { conditions: ['development'] },
  ssr: { resolve: { conditions: ['development'], externalConditions: ['development'] } },
  test: {
    include: ['packages/*/test/**/*.test.ts'],
    environment: 'node',
    globalSetup: ['./vitest.global-setup.ts'],
    testTimeout: 20_000,
    hookTimeout: 60_000,
  },
});
