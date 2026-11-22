import js from '@eslint/js';
import { defineConfig, globalIgnores } from 'eslint/config';
import globals from 'globals';
import tseslint from 'typescript-eslint';

export default defineConfig([
  globalIgnores(['**/dist', '**/node_modules', 'packages/shared/src/generated', 'packages/shared/wasm']),
  {
    files: ['**/*.{js,mjs,ts}'],
    extends: [js.configs.recommended, tseslint.configs.recommended],
    languageOptions: { ecmaVersion: 2023, globals: { ...globals.node } },
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/consistent-type-imports': 'error',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
      // Secret values must never reach a log line (design §7.3): processes log
      // through shared/src/log.ts, which takes fixed fields only.
      'no-console': 'error',
    },
  },
  {
    // Tooling scripts and the image entry point print to the terminal.
    files: ['scripts/**', 'bin/**', 'vitest.global-setup.ts'],
    rules: { 'no-console': 'off' },
  },
]);
