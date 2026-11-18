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
    restoreMocks: true,
    unstubGlobals: true,
  },
});
