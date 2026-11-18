// Runs before `astro build`. Copies, into public/ (both copies git-ignored):
// - Scalar's standalone browser bundle, so the API explorer loads it from
//   this site with no CDN (the version is pinned in package.json);
// - the OpenAPI contract, api/openapi.draft.json, so the explorer always
//   renders the contract in this checkout.
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const site = join(dirname(fileURLToPath(import.meta.url)), '..');
const repo = join(site, '..');
const out = join(site, 'public/api');
mkdirSync(out, { recursive: true });
copyFileSync(
  join(site, 'node_modules/@scalar/api-reference/dist/browser/standalone.js'),
  join(out, 'scalar.js'),
);
copyFileSync(join(repo, 'api/openapi.draft.json'), join(out, 'openapi.json'));
