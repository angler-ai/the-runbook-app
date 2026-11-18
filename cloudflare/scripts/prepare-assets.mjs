// Copy the built web UI (../web/dist) to build-assets/ with the `_headers`
// file (CSP, caching) the assets engine applies. Run before `wrangler deploy`:
//   (cd ../web && node scripts/build-wasm.mjs && npm run build) && node scripts/prepare-assets.mjs
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { prepareAssets } from './dev-stack.mjs';

const here = dirname(dirname(fileURLToPath(import.meta.url)));
await prepareAssets(join(here, '..', 'web', 'dist'), join(here, 'build-assets'));
console.log('build-assets ready');
