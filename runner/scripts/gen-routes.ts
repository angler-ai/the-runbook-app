// Writes packages/shared/src/contracts/public-routes.json from public.ts.
//   npm run gen:routes            write the file
//   node … gen-routes.ts --check  fail if the committed file has drifted
// The core embeds a copy (crates/runbook-api/src/runner_routes.json, Task R0-5)
// and fails a test when the two drift.
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { renderPublicRoutes } from '../packages/shared/src/contracts/public.ts';

const target = new URL('../packages/shared/src/contracts/public-routes.json', import.meta.url);
const generated = renderPublicRoutes();

if (process.argv.includes('--check')) {
  const committed = await readFile(target, 'utf8').catch(() => '');
  if (committed !== generated) {
    console.error(`${fileURLToPath(target)} is out of date. Run \`npm run gen:routes\` and commit it.`);
    process.exit(1);
  }
  console.log('gen:routes: public-routes.json is current');
} else {
  await writeFile(target, generated);
  console.log(`gen:routes: wrote ${fileURLToPath(target)}`);
}
