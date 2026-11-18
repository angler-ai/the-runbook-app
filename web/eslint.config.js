import js from '@eslint/js';
import { defineConfig, globalIgnores } from 'eslint/config';
import jsxA11y from 'eslint-plugin-jsx-a11y';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import tseslint from 'typescript-eslint';

// A10: untrusted content is rendered as text; raw HTML injection is banned outright.
const noRawHtml = [
  {
    selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
    message: 'Render untrusted content as text (A10). dangerouslySetInnerHTML is banned.',
  },
  {
    selector: "Property[key.name='dangerouslySetInnerHTML']",
    message: 'Render untrusted content as text (A10). dangerouslySetInnerHTML is banned.',
  },
];

// A12: src/ui and src/lib are presentational (props only, no fetching, no router).
const boundaryMessage =
  'src/ui and src/lib take props only (A12): no imports from src/api, src/features or src/routes, no router, no data fetching.';

export default defineConfig([
  globalIgnores(['dist', 'src/api/schema.d.ts', 'playwright-report', 'test-results']),
  {
    files: ['**/*.{js,mjs,ts,tsx}'],
    extends: [js.configs.recommended, tseslint.configs.recommended],
    languageOptions: { ecmaVersion: 2023, globals: { ...globals.browser } },
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/consistent-type-imports': 'error',
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],
      'no-restricted-syntax': ['error', ...noRawHtml],
    },
  },
  {
    files: ['src/**/*.{ts,tsx}'],
    extends: [jsxA11y.flatConfigs.strict, reactHooks.configs.flat.recommended],
  },
  {
    files: ['src/ui/**/*.{ts,tsx}', 'src/lib/**/*.{ts,tsx}'],
    rules: {
      '@typescript-eslint/no-restricted-imports': [
        'error',
        {
          paths: [
            { name: '@tanstack/react-router', message: boundaryMessage },
            { name: '@tanstack/react-query', message: boundaryMessage },
            { name: 'openapi-fetch', message: boundaryMessage },
          ],
          patterns: [
            {
              regex: '(^|/)(api(?!/schema(\\.d)?(\\.ts)?$)|features|routes)(/|$)',
              message: boundaryMessage,
            },
            { regex: '^@tanstack/(react-router/|router-)', message: boundaryMessage },
            // The generated wire types are shared vocabulary: `import type` only.
            {
              regex: '(^|/)api/schema(\\.d)?(\\.ts)?$',
              allowTypeImports: true,
              message: 'Import the generated API schema with `import type` only (A12).',
            },
          ],
        },
      ],
      'no-restricted-globals': [
        'error',
        { name: 'fetch', message: boundaryMessage },
        { name: 'XMLHttpRequest', message: boundaryMessage },
        { name: 'EventSource', message: boundaryMessage },
        { name: 'WebSocket', message: boundaryMessage },
      ],
    },
  },
  {
    files: ['*.{js,ts}', 'scripts/**/*.mjs', 'src/**/*.node.test.ts', 'e2e/**/*.ts'],
    languageOptions: { globals: { ...globals.node } },
  },
]);
