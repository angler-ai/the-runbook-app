import react from '@vitejs/plugin-react';
import { configDefaults, defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.{ts,tsx}'],
    // *.node.test.ts files run under `node --test` (dependency-free tests), not Vitest.
    exclude: [...configDefaults.exclude, '**/*.node.test.ts', 'e2e/**'],
    setupFiles: ['./src/test/setup.ts'],
    // The builder and run-page tests drive whole pages in jsdom; shared CI
    // machines run them several times slower than a laptop.
    testTimeout: 20_000,
    restoreMocks: true,
    unstubGlobals: true,
  },
});
