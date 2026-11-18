// Keeps the review stack's "running" post-deploy run looking live: the
// agent's claim on its latency step would lapse after the 10-minute agent
// lease, so this heartbeats it every 4 minutes with the agent's own stored
// execution pair (`runbook step heartbeat`). Started and stopped by
// scripts/dev/review.sh; reads `running.json` (IDs only) from the state dir.
//
//   REVIEW_STATE_DIR=... RUNBOOK_BIN=... REVIEW_API_URL=... node keep-claimed.mjs
import { execFile } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const state = process.env.REVIEW_STATE_DIR;
const cli = process.env.RUNBOOK_BIN;
const api = process.env.REVIEW_API_URL;
if (!state || !cli || !api) throw new Error('REVIEW_STATE_DIR, RUNBOOK_BIN and REVIEW_API_URL are required');
const running = JSON.parse(readFileSync(join(state, 'running.json'), 'utf8'));
const env = { ...process.env, XDG_CONFIG_HOME: join(state, 'config', running.who) };
delete env.RUNBOOK_TOKEN;
delete env.RUNBOOK_HOST;

function heartbeat() {
  execFile(cli, ['step', 'heartbeat', running.step_run_id, '--host', api], { env }, (error, _out, stderr) => {
    const when = new Date().toISOString();
    if (error) {
      // The run may have been finished or cancelled from the UI: stop quietly.
      process.stdout.write(`${when} heartbeat stopped: ${stderr.trim() || error.message}\n`);
      process.exit(0);
    }
    process.stdout.write(`${when} heartbeat ${running.step_run_id}\n`);
  });
}
heartbeat();
setInterval(heartbeat, 4 * 60 * 1000);
process.on('SIGTERM', () => process.exit(0));
process.on('SIGINT', () => process.exit(0));
