// `npm run e2e:oauth [-- <playwright args>]`: the MCP2 OAuth browser test on
// both editions. The native project creates and drops its own
// rb_test_oauth_* database, so run it under the shared Postgres lock, as the
// auth suites are (this script takes no lock itself, so wrapping it can
// never deadlock):
//
//   DATABASE_URL=… lockf <repo>/../.locks/pg.lock npm run e2e:oauth
//
// Needs DATABASE_URL, a built runbook-server (CARGO_TARGET_DIR or target/),
// auth/dist and the test Worker (cloudflare/build-test).
import { spawn } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const web = join(here, '..', '..');
const cli = join(web, 'node_modules', '@playwright', 'test', 'cli.js');
const child = spawn(
  process.execPath,
  [cli, 'test', '-c', join(here, 'playwright.config.ts'), ...process.argv.slice(2)],
  { cwd: web, stdio: 'inherit' },
);
child.on('exit', (code, signal) => process.exit(code ?? (signal ? 130 : 1)));
