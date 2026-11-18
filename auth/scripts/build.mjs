// Bundles both entry points with esbuild:
//   dist/worker.mjs  Workers module (D1), the shape `wrangler deploy` would upload;
//   dist/server.mjs  Node/Bun module (Postgres), run as `node dist/server.mjs` or
//                    `bun dist/server.mjs`; it reads migrations from ../drizzle/pg.
import { build } from 'esbuild';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const common = { absWorkingDir: root, bundle: true, format: 'esm', target: 'es2022', logLevel: 'warning', legalComments: 'none' };

await build({
  ...common,
  entryPoints: ['src/worker.ts'],
  outfile: 'dist/worker.mjs',
  platform: 'browser',
  conditions: ['workerd', 'worker', 'browser'],
  external: ['node:*', 'cloudflare:*'],
});

await build({
  ...common,
  entryPoints: ['src/server.ts'],
  outfile: 'dist/server.mjs',
  platform: 'node',
  target: 'node22',
  // nodemailer is CommonJS and calls require(); give the ESM bundle one.
  banner: { js: "import { createRequire as __runbookCreateRequire } from 'node:module'; const require = __runbookCreateRequire(import.meta.url);" },
});
